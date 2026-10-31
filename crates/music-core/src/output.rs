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

// ---- a tidy list for the picker ----------------------------------------------------------
//
// ALSA registers dozens of generic PCMs in alsa.conf whatever hardware is present (rate
// converters, plugin wrappers, per-card surround / S/PDIF variants), and cpal's hint
// enumeration lists every one; its physical-device probe then adds each card again as
// raw `hw:` / `plughw:`. Idea and list from Zuno (Apache-2.0, `src-tauri/src/audio.rs`,
// `is_alsa_plugin_noise`); unlike Zuno, a card that has *only* raw entries keeps its best
// one instead of becoming unselectable.

/// ALSA PCM types that are never a sensible pick by themselves.
const ALSA_NOISE: &[&str] = &[
    "null",
    "jack",
    "oss",
    "speex",
    "speexrate",
    "lavrate",
    "samplerate",
    "upmix",
    "vdownmix",
    "surround21",
    "surround40",
    "surround41",
    "surround50",
    "surround51",
    "surround71",
    "iec958",
    "spdif",
    "usbstream",
    "dsnoop",
    "rear",
    "center_lfe",
    "side",
    "modem",
    "phoneline",
    "shm",
    "tee",
    "file",
    "empty",
    "rate",
    "route",
    "mulaw",
    "alaw",
    "adpcm",
    "plug",
    "multi",
    "ladspa",
    "asym",
    "a52",
    "dcaenc",
    "pcm",
];

/// Per-card ALSA types, best first: the first one present for a card is kept, the rest
/// are duplicates of it.
const ALSA_CARD_RANK: &[&str] = &["sysdefault", "hdmi", "dmix", "front", "plughw", "hw"];

/// The ALSA PCM type of a cpal id (`alsa:hw:CARD=PCH,DEV=0` → `hw`); `None` elsewhere.
fn alsa_type(id: &str) -> Option<&str> {
    let pcm = id.strip_prefix("alsa:")?;
    Some(pcm.split(':').next().unwrap_or(pcm))
}

/// Names for ALSA PCMs that come without a description.
fn alsa_friendly(pcm_type: &str) -> Option<&'static str> {
    Some(match pcm_type {
        "default" => "Default (ALSA)",
        "pipewire" => "PipeWire",
        "pulse" => "PulseAudio",
        _ => return None,
    })
}

/// The output devices worth offering: on Linux without ALSA's virtual and duplicate
/// PCMs (one entry per card), everywhere without repeated ids, with friendly names, the
/// system default first. `show_all` keeps every device (still de-duplicated by id);
/// `keep` (the device the user picked) is never dropped.
pub fn tidy(devices: Vec<OutputDevice>, show_all: bool, keep: Option<&str>) -> Vec<OutputDevice> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<OutputDevice> = devices
        .into_iter()
        .filter(|d| seen.insert(d.id.clone()))
        .map(|mut d| {
            if d.name.trim().is_empty() || Some(d.name.as_str()) == d.id.strip_prefix("alsa:") {
                if let Some(name) = alsa_type(&d.id).and_then(alsa_friendly) {
                    d.name = name.to_owned();
                } else if d.name.trim().is_empty() {
                    d.name = d.id.clone();
                }
            }
            d
        })
        .collect();
    if !show_all {
        let kept = |d: &OutputDevice| keep == Some(d.id.as_str()) || d.is_default;
        out.retain(|d| kept(d) || !alsa_type(&d.id).is_some_and(|t| ALSA_NOISE.contains(&t)));
        // One entry per card: the card's entries share the description's first line.
        let rank = |d: &OutputDevice| {
            alsa_type(&d.id).and_then(|t| ALSA_CARD_RANK.iter().position(|r| *r == t))
        };
        let best: std::collections::HashMap<String, usize> =
            out.iter().fold(Default::default(), |mut best, d| {
                if let Some(r) = rank(d) {
                    let e = best.entry(d.name.clone()).or_insert(r);
                    *e = (*e).min(r);
                }
                best
            });
        out.retain(|d| match rank(d) {
            Some(r) => kept(d) || best.get(&d.name) == Some(&r),
            None => true,
        });
    }
    // Same name twice (two identical USB headsets): tell them apart.
    let mut counts: std::collections::HashMap<String, usize> = Default::default();
    for d in &out {
        *counts.entry(d.name.clone()).or_default() += 1;
    }
    let mut nth: std::collections::HashMap<String, usize> = Default::default();
    for d in &mut out {
        if counts[&d.name] > 1 {
            let n = nth.entry(d.name.clone()).or_default();
            *n += 1;
            if *n > 1 {
                d.name = format!("{} ({n})", d.name);
            }
        }
    }
    // The default first, the rest as the system lists them.
    out.sort_by_key(|d| !d.is_default);
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str) -> OutputDevice {
        OutputDevice {
            id: id.into(),
            name: name.into(),
            is_default: false,
        }
    }

    fn ids(list: &[OutputDevice]) -> Vec<&str> {
        list.iter().map(|d| d.id.as_str()).collect()
    }

    /// What cpal lists on a typical PipeWire desktop with onboard audio + HDMI.
    fn alsa() -> Vec<OutputDevice> {
        let mut default = dev(
            "alsa:default",
            "Default ALSA Output (currently PipeWire Media Server)",
        );
        default.is_default = true;
        vec![
            dev(
                "alsa:null",
                "Discard all samples (playback) or generate zero samples (capture)",
            ),
            dev(
                "alsa:samplerate",
                "Rate Converter Plugin Using Samplerate Library",
            ),
            dev(
                "alsa:speexrate",
                "Rate Converter Plugin Using Speex Resampler",
            ),
            dev("alsa:jack", "JACK Audio Connection Kit"),
            dev("alsa:oss", "Open Sound System"),
            dev("alsa:pipewire", "pipewire"),
            dev("alsa:pulse", "PulseAudio Sound Server"),
            dev("alsa:upmix", "Plugin for channel upmix (4,6,8)"),
            dev(
                "alsa:vdownmix",
                "Plugin for channel downmix (stereo) with a simple spacialization",
            ),
            default,
            dev("alsa:sysdefault:CARD=PCH", "HDA Intel PCH, ALC892 Analog"),
            dev("alsa:front:CARD=PCH,DEV=0", "HDA Intel PCH, ALC892 Analog"),
            dev(
                "alsa:surround21:CARD=PCH,DEV=0",
                "HDA Intel PCH, ALC892 Analog",
            ),
            dev(
                "alsa:surround51:CARD=PCH,DEV=0",
                "HDA Intel PCH, ALC892 Analog",
            ),
            dev(
                "alsa:iec958:CARD=PCH,DEV=0",
                "HDA Intel PCH, ALC892 Digital",
            ),
            dev("alsa:dmix:CARD=PCH,DEV=0", "HDA Intel PCH, ALC892 Analog"),
            dev("alsa:hdmi:CARD=HDMI,DEV=0", "HDA Intel HDMI, HDMI 0"),
            dev("alsa:dmix:CARD=HDMI,DEV=3", "HDA Intel HDMI, HDMI 0"),
            dev("alsa:hw:CARD=PCH,DEV=0", "HDA Intel PCH, ALC892 Analog"),
            dev("alsa:plughw:CARD=PCH,DEV=0", "HDA Intel PCH, ALC892 Analog"),
            dev("alsa:hw:CARD=HDMI,DEV=3", "HDA Intel HDMI, HDMI 0"),
            // A USB DAC with no sysdefault / hdmi hint: only raw entries.
            dev("alsa:hw:CARD=2,DEV=0", "USB Audio DAC, USB Audio"),
            dev("alsa:plughw:CARD=2,DEV=0", "USB Audio DAC, USB Audio"),
        ]
    }

    #[test]
    fn alsa_noise_and_duplicates_are_dropped() {
        let list = tidy(alsa(), false, None);
        assert_eq!(
            ids(&list),
            [
                "alsa:default",
                "alsa:pipewire",
                "alsa:pulse",
                "alsa:sysdefault:CARD=PCH",
                "alsa:hdmi:CARD=HDMI,DEV=0",
                "alsa:plughw:CARD=2,DEV=0",
            ]
        );
        assert!(list[0].is_default, "the default comes first");
        assert_eq!(list[1].name, "PipeWire", "a friendly name for a bare PCM");
    }

    #[test]
    fn show_all_and_the_picked_device_stay() {
        let all = tidy(alsa(), true, None);
        assert_eq!(all.len(), alsa().len());
        assert_eq!(all[0].id, "alsa:default");
        // A device picked earlier is kept even if it would be filtered.
        let list = tidy(alsa(), false, Some("alsa:hw:CARD=PCH,DEV=0"));
        assert!(ids(&list).contains(&"alsa:hw:CARD=PCH,DEV=0"));
        assert!(ids(&list).contains(&"alsa:sysdefault:CARD=PCH"));
    }

    #[test]
    fn other_systems_are_cleaned_but_not_filtered() {
        let mut speakers = dev("coreaudio:BuiltInSpeakerDevice", "MacBook Pro Speakers");
        speakers.is_default = true;
        let list = tidy(
            vec![
                dev("coreaudio:BlackHole2ch_UID", "BlackHole 2ch"),
                speakers.clone(),
                speakers.clone(),
                dev("coreaudio:A", "USB Headset"),
                dev("coreaudio:B", "USB Headset"),
                dev("wasapi:{x}", ""),
            ],
            false,
            None,
        );
        assert_eq!(
            list.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            [
                "MacBook Pro Speakers",
                "BlackHole 2ch",
                "USB Headset",
                "USB Headset (2)",
                "wasapi:{x}"
            ]
        );
    }
}
