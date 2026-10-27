//! Microphone capture (cpal input, through rodio's re-export) for recognition.
//!
//! [`record_microphone`] blocks the calling thread for up to `length`, reports the
//! input level every ~50 ms, stops as soon as `cancel` is set, and returns 16 kHz
//! mono PCM. On macOS the OS asks for microphone permission the first time (the app
//! bundle needs `NSMicrophoneUsageDescription`); a denied permission shows up as an
//! error or, with some drivers, as pure digital silence, which is reported as
//! [`RecognitionError::Silent`].

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::cpal::{SampleFormat, StreamConfig};

use super::RecognitionError;
use super::audio::{rms, to_signature_pcm};

/// Progress of a capture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureProgress {
    pub elapsed: Duration,
    pub length: Duration,
    /// Input level of the last ~50 ms (RMS, 0..1).
    pub level: f32,
}

/// Name of the default input device, if there is one.
pub fn default_input_name() -> Option<String> {
    let device = rodio::cpal::default_host().default_input_device()?;
    Some(
        device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "Microphone".into()),
    )
}

/// Records `length` from the default input device. Returns early with
/// [`RecognitionError::Cancelled`] when `cancel` becomes true.
pub fn record_microphone(
    length: Duration,
    cancel: &AtomicBool,
    mut progress: impl FnMut(CaptureProgress),
) -> Result<Vec<i16>, RecognitionError> {
    let device = rodio::cpal::default_host()
        .default_input_device()
        .ok_or(RecognitionError::NoMicrophone)?;
    let supported = device
        .default_input_config()
        .map_err(|e| capture_error(&e))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let channels = config.channels;
    let rate = config.sample_rate;

    let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(
        (length.as_secs_f64() * f64::from(rate) * f64::from(channels)) as usize + 4096,
    )));
    let level = Arc::new(AtomicU32::new(0));
    let failed: Arc<Mutex<Option<String>>> = Arc::default();

    let push = {
        let buffer = buffer.clone();
        let level = level.clone();
        move |samples: &mut dyn Iterator<Item = f32>| {
            let mut buf = buffer.lock().unwrap_or_else(PoisonError::into_inner);
            let start = buf.len();
            buf.extend(samples);
            level.store(rms(&buf[start..]).to_bits(), Ordering::Relaxed);
        }
    };
    let on_error = {
        let failed = failed.clone();
        move |e: rodio::cpal::Error| {
            *failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
        }
    };
    let timeout = Some(Duration::from_secs(3));
    let stream = match format {
        SampleFormat::F32 => device.build_input_stream::<f32, _, _>(
            config,
            move |data: &[f32], _| push(&mut data.iter().copied()),
            on_error,
            timeout,
        ),
        SampleFormat::I16 => device.build_input_stream::<i16, _, _>(
            config,
            move |data: &[i16], _| push(&mut data.iter().map(|&s| f32::from(s) / 32768.0)),
            on_error,
            timeout,
        ),
        SampleFormat::I32 => device.build_input_stream::<i32, _, _>(
            config,
            move |data: &[i32], _| {
                push(
                    &mut data
                        .iter()
                        .map(|&s| (f64::from(s) / 2_147_483_648.0) as f32),
                )
            },
            on_error,
            timeout,
        ),
        SampleFormat::U16 => device.build_input_stream::<u16, _, _>(
            config,
            move |data: &[u16], _| {
                push(&mut data.iter().map(|&s| (f32::from(s) - 32768.0) / 32768.0))
            },
            on_error,
            timeout,
        ),
        other => {
            return Err(RecognitionError::Capture(format!(
                "unsupported input sample format {other:?}"
            )));
        }
    }
    .map_err(|e| capture_error(&e))?;
    stream.play().map_err(|e| capture_error(&e))?;

    let started = Instant::now();
    let result = loop {
        std::thread::sleep(Duration::from_millis(50));
        if cancel.load(Ordering::Relaxed) {
            break Err(RecognitionError::Cancelled);
        }
        if let Some(e) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
            break Err(RecognitionError::Capture(e));
        }
        let elapsed = started.elapsed().min(length);
        progress(CaptureProgress {
            elapsed,
            length,
            level: f32::from_bits(level.load(Ordering::Relaxed)),
        });
        if elapsed >= length {
            break Ok(());
        }
    };
    drop(stream); // stops the device before the (possibly slow) conversion
    result?;
    let samples = std::mem::take(&mut *buffer.lock().unwrap_or_else(PoisonError::into_inner));
    if samples.is_empty() {
        return Err(RecognitionError::Capture(
            "the microphone delivered no audio".into(),
        ));
    }
    if samples.iter().all(|&s| s == 0.0) {
        return Err(RecognitionError::Silent);
    }
    Ok(to_signature_pcm(&samples, channels, rate))
}

fn capture_error(e: &rodio::cpal::Error) -> RecognitionError {
    use rodio::CpalErrorKind as K;
    match e.kind() {
        K::PermissionDenied => RecognitionError::PermissionDenied,
        K::DeviceNotAvailable => RecognitionError::NoMicrophone,
        _ => RecognitionError::Capture(e.to_string()),
    }
}
