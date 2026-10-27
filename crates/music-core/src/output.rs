//! Output devices: listing, opening by persisted id or name, falling back to
//! the system default.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::{DeviceSinkBuilder, MixerDeviceSink};

use crate::{Error, Result};

/// Some drivers never return from opening a stream (seen with a stalled
/// virtual device); give up on them after this long.
const OPEN_TIMEOUT: Duration = Duration::from_secs(3);

/// An audio output device, as listed by [`crate::Player::output_devices`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OutputDevice {
    /// Stable identifier to persist and pass to
    /// [`crate::Player::set_output_device`] (cpal's `DeviceId`, e.g.
    /// `coreaudio:<uid>`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// The current system default output.
    pub is_default: bool,
}

fn describe(device: &rodio::Device) -> Option<(String, String)> {
    let id = device.id().ok()?.to_string();
    let name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| id.clone());
    Some((id, name))
}

pub(crate) fn list() -> Vec<OutputDevice> {
    let host = rodio::cpal::default_host();
    let default_id = host
        .default_output_device()
        .and_then(|d| d.id().ok())
        .map(|id| id.to_string());
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| describe(&d))
        .map(|(id, name)| OutputDevice {
            is_default: default_id.as_deref() == Some(id.as_str()),
            id,
            name,
        })
        .collect()
}

/// Finds a device by id, or failing that by name (case-insensitive).
fn find(wanted: &str) -> Option<rodio::Device> {
    let host = rodio::cpal::default_host();
    let devices: Vec<_> = host.output_devices().ok()?.collect();
    let by_id = devices
        .iter()
        .position(|d| describe(d).is_some_and(|(id, _)| id == wanted));
    let index = by_id.or_else(|| {
        devices
            .iter()
            .position(|d| describe(d).is_some_and(|(_, name)| name.eq_ignore_ascii_case(wanted)))
    })?;
    devices.into_iter().nth(index)
}

/// True if `wanted` (an id or name) names a device that is present.
pub(crate) fn is_present(wanted: &str) -> bool {
    find(wanted).is_some()
}

/// Opens `device` on a helper thread so a hanging driver cannot block the engine.
fn open_device<E>(device: rodio::Device, on_error: E) -> Result<MixerDeviceSink>
where
    E: FnMut(rodio::cpal::Error) + Send + Clone + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("music-core-device-open".into())
        .spawn(move || {
            let sink = DeviceSinkBuilder::from_device(device)
                .map(|b| b.with_error_callback(on_error))
                .and_then(|b| b.open_sink_or_fallback());
            // If the engine gave up waiting, the sink is closed again here.
            let _ = tx.send(sink);
        })
        .map_err(|e| Error::Audio(e.to_string()))?;
    match rx.recv_timeout(OPEN_TIMEOUT) {
        Ok(Ok(mut sink)) => {
            sink.log_on_drop(false);
            Ok(sink)
        }
        Ok(Err(e)) => Err(Error::Audio(e.to_string())),
        Err(_) => Err(Error::Audio("the device did not respond".into())),
    }
}

/// Opens `wanted` (id or name), or the system default when it is `None` or
/// missing. Returns the sink, the device's display name, and whether the
/// wanted device was used.
pub(crate) fn open<E>(wanted: Option<&str>, on_error: E) -> Result<(MixerDeviceSink, String, bool)>
where
    E: FnMut(rodio::cpal::Error) + Send + Clone + 'static,
{
    if let Some(device) = wanted.and_then(find) {
        let name = describe(&device).map(|(_, n)| n).unwrap_or_default();
        match open_device(device, on_error.clone()) {
            Ok(sink) => return Ok((sink, name, true)),
            Err(e) => log::warn!("could not open output device {name:?}: {e}; using the default"),
        }
    }
    let device = rodio::cpal::default_host()
        .default_output_device()
        .ok_or_else(|| Error::Audio("no output device".into()))?;
    let name = describe(&device).map(|(_, n)| n).unwrap_or_default();
    let sink = open_device(device, on_error)?;
    Ok((sink, name, wanted.is_none()))
}

/// Closes a stream without blocking the caller (a vanished device may stall).
pub(crate) fn close(sink: MixerDeviceSink) {
    let _ = thread::Builder::new()
        .name("music-core-device-close".into())
        .spawn(move || drop(sink));
}

/// Errors after which the stream is dead and must be reopened.
pub(crate) fn is_fatal(kind: rodio::CpalErrorKind) -> bool {
    use rodio::CpalErrorKind as K;
    matches!(
        kind,
        K::DeviceNotAvailable | K::StreamInvalidated | K::HostUnavailable
    )
}
