//! Media Foundation: the H.264 decoder that ships with Windows 10 / 11
//! (`CLSID_MSH264DecoderMFT`, used synchronously), no extra library. It takes
//! Annex B (start codes, SPS / PPS in front of every keyframe), returns NV12
//! in display order with the input times, which is converted to BGRA here.
//!
//! Without a Direct3D device manager the decoder runs on the CPU (it is
//! multi-threaded and SIMD-optimised; a 480p stream takes a few percent of one
//! core). Handing it a D3D11 device for DXVA is the next step
//! (docs/BACKLOG.md). Windows "N" editions without the Media Feature Pack have
//! no decoder: [`available`] is false there and the setting is hidden.

use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, IMF2DBuffer, IMFMediaType, IMFSample, IMFTransform, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
    MFSTARTUP_NOSOCKET, MFStartup, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::core::Interface;

use super::{Decoder, Picture, Yuv};
use crate::video::mp4::{InitSegment, Sample};
use crate::{Error, Result};

fn mf_err(what: &str, e: windows::core::Error) -> Error {
    Error::Unplayable(format!("Media Foundation: {what}: {e}"))
}

/// COM on this thread and Media Foundation in the process.
fn start() -> Result<()> {
    // SAFETY: plain initialisation calls; a thread already in another
    // apartment just keeps it (the decoder is free-threaded).
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    static STARTED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.map_err(|e| e.to_string())
        })
        .clone()
        .map_err(|e| Error::Unplayable(format!("Media Foundation: {e}")))
}

/// Whether the system has the H.264 decoder (checked once).
pub(crate) fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let ok = start().is_ok() && create().is_ok();
        if !ok {
            log::info!("video: no Media Foundation H.264 decoder");
        }
        ok
    })
}

fn create() -> Result<IMFTransform> {
    // SAFETY: COM is initialised on this thread (`start`).
    unsafe { CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| mf_err("creating the H.264 decoder", e))
}

pub(crate) struct MfDecoder {
    transform: IMFTransform,
    init: InitSegment,
    /// The output's coded size and row pitch (NV12).
    coded: (u32, u32),
    stride: u32,
    /// The decoder allocates its output samples.
    provides_samples: bool,
    output_size: u32,
    /// Drained at the end of the video: a flush alone doesn't bring it back.
    drained: bool,
}

// SAFETY: the transform is created in the multi-threaded apartment and only
// used from the video worker thread.
unsafe impl Send for MfDecoder {}

impl MfDecoder {
    pub(crate) fn new(init: &InitSegment) -> Result<Self> {
        start()?;
        let transform = create()?;
        // SAFETY: COM calls on a valid transform and media types.
        unsafe {
            // Low latency: a frame comes out as soon as it can be shown instead of
            // after the decoder's reorder buffer fills (up to 16 frames by default,
            // which a flush on seek would throw away).
            if let Ok(attributes) = transform.GetAttributes() {
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let input = MFCreateMediaType().map_err(|e| mf_err("input type", e))?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .and_then(|()| input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264))
                .and_then(|()| {
                    input.SetUINT64(
                        &MF_MT_FRAME_SIZE,
                        (u64::from(init.width) << 32) | u64::from(init.height),
                    )
                })
                .and_then(|()| {
                    input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                })
                .and_then(|()| transform.SetInputType(0, &input, 0))
                .map_err(|e| mf_err("setting the input type", e))?;
        }
        let mut decoder = Self {
            transform,
            init: init.clone(),
            coded: (init.width, init.height),
            stride: init.width,
            provides_samples: false,
            output_size: 0,
            drained: false,
        };
        decoder.negotiate_output()?;
        // SAFETY: valid transform.
        unsafe {
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .and_then(|()| {
                    decoder
                        .transform
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                })
                .map_err(|e| mf_err("starting", e))?;
        }
        Ok(decoder)
    }

    /// Picks NV12 output (again after a format change) and reads its geometry.
    fn negotiate_output(&mut self) -> Result<()> {
        // SAFETY: COM calls on a valid transform.
        unsafe {
            let mut chosen: Option<IMFMediaType> = None;
            for i in 0.. {
                let Ok(t) = self.transform.GetOutputAvailableType(0, i) else {
                    break;
                };
                if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                    chosen = Some(t);
                    break;
                }
            }
            let t = chosen
                .ok_or_else(|| Error::Unplayable("Media Foundation: no NV12 output".into()))?;
            self.transform
                .SetOutputType(0, &t, 0)
                .map_err(|e| mf_err("setting the output type", e))?;
            if let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                self.coded = ((size >> 32) as u32, size as u32);
            }
            self.stride = t
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .map(|s| (s as i32).unsigned_abs())
                .unwrap_or(self.coded.0);
            let info = self
                .transform
                .GetOutputStreamInfo(0)
                .map_err(|e| mf_err("output stream info", e))?;
            self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            self.output_size = info.cbSize;
        }
        Ok(())
    }

    fn input_sample(&self, sample: &Sample) -> Result<IMFSample> {
        let mut data = Vec::with_capacity(sample.data.len() + 64);
        if sample.keyframe {
            data.extend(self.init.avc.annex_b_parameter_sets());
        }
        data.extend(
            self.init
                .avc
                .to_annex_b(&sample.data)
                .ok_or_else(|| Error::Parse("video: broken NAL unit lengths".into()))?,
        );
        // SAFETY: the buffer is locked while `data` is copied into it.
        unsafe {
            let buffer =
                MFCreateMemoryBuffer(data.len() as u32).map_err(|e| mf_err("buffer", e))?;
            let mut ptr = std::ptr::null_mut();
            buffer
                .Lock(&mut ptr, None, None)
                .map_err(|e| mf_err("lock", e))?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock().map_err(|e| mf_err("unlock", e))?;
            buffer
                .SetCurrentLength(data.len() as u32)
                .map_err(|e| mf_err("length", e))?;
            let s = MFCreateSample().map_err(|e| mf_err("sample", e))?;
            s.AddBuffer(&buffer).map_err(|e| mf_err("add buffer", e))?;
            s.SetSampleTime((sample.pts.as_nanos() / 100) as i64)
                .and_then(|()| s.SetSampleDuration((sample.duration.as_nanos() / 100) as i64))
                .map_err(|e| mf_err("times", e))?;
            Ok(s)
        }
    }

    /// Everything the decoder has ready.
    fn drain_output(&mut self, out: &mut Vec<(Duration, Picture)>) -> Result<()> {
        loop {
            // SAFETY: the output buffer struct owns the sample references it
            // holds; both are taken out of their ManuallyDrop below.
            let sample = if self.provides_samples {
                None
            } else {
                unsafe {
                    let s = MFCreateSample().map_err(|e| mf_err("sample", e))?;
                    let b = MFCreateMemoryBuffer(
                        self.output_size.max(self.coded.0 * self.coded.1 * 3 / 2),
                    )
                    .map_err(|e| mf_err("buffer", e))?;
                    s.AddBuffer(&b).map_err(|e| mf_err("add buffer", e))?;
                    Some(s)
                }
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            let [buffer] = buffers;
            let sample = std::mem::ManuallyDrop::into_inner(buffer.pSample);
            drop(std::mem::ManuallyDrop::into_inner(buffer.pEvents));
            match result {
                Ok(()) => {
                    if let Some(sample) = sample
                        && let Some(picture) = self.picture(&sample)?
                    {
                        out.push(picture);
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.negotiate_output()?,
                Err(e) => return Err(mf_err("decoding", e)),
            }
        }
    }

    fn picture(&self, sample: &IMFSample) -> Result<Option<(Duration, Picture)>> {
        // SAFETY: the buffer is locked while it is read.
        unsafe {
            let pts = sample.GetSampleTime().unwrap_or(0).max(0) as u64;
            let buffer = sample
                .ConvertToContiguousBuffer()
                .map_err(|e| mf_err("output buffer", e))?;
            let (width, height) = (
                self.init.width.min(self.coded.0) as usize,
                self.init.height.min(self.coded.1) as usize,
            );
            let (coded_h, mut stride) = (self.coded.1 as usize, self.stride as usize);
            let two_d: Option<IMF2DBuffer> = buffer.cast().ok();
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            let locked_2d = match &two_d {
                Some(b) => {
                    let mut pitch = 0i32;
                    let ok = b.Lock2D(&mut ptr, &mut pitch).is_ok() && pitch > 0;
                    if ok {
                        stride = pitch as usize;
                        len = (stride * coded_h * 3 / 2) as u32;
                    }
                    ok
                }
                None => false,
            };
            if !locked_2d {
                buffer
                    .Lock(&mut ptr, None, Some(&mut len))
                    .map_err(|e| mf_err("lock", e))?;
            }
            let data = std::slice::from_raw_parts(ptr, len as usize);
            let picture = (data.len() >= stride * coded_h * 3 / 2).then(|| {
                let (y, uv) = data.split_at(stride * coded_h);
                Yuv {
                    width,
                    height,
                    y,
                    y_stride: stride,
                    u: uv,
                    v: uv,
                    uv_stride: stride,
                    interleaved: true,
                }
                .to_bgra()
            });
            match (&two_d, locked_2d) {
                (Some(b), true) => {
                    let _ = b.Unlock2D();
                }
                _ => {
                    let _ = buffer.Unlock();
                }
            }
            Ok(picture.map(|data| {
                (
                    Duration::from_nanos(pts * 100),
                    Picture::Bgra {
                        width: width as u32,
                        height: height as u32,
                        data,
                    },
                )
            }))
        }
    }
}

impl Decoder for MfDecoder {
    fn decode(&mut self, sample: &Sample) -> Result<Vec<(Duration, Picture)>> {
        let input = self.input_sample(sample)?;
        let mut out = Vec::new();
        // SAFETY: valid transform and sample.
        match unsafe { self.transform.ProcessInput(0, &input, 0) } {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_NOTACCEPTING => {
                // Full: take what is ready, then it accepts the sample.
                self.drain_output(&mut out)?;
                unsafe { self.transform.ProcessInput(0, &input, 0) }
                    .map_err(|e| mf_err("input", e))?;
            }
            Err(e) => return Err(mf_err("input", e)),
        }
        self.drain_output(&mut out)?;
        Ok(out)
    }

    fn flush(&mut self) {
        // After a drain (the end of the video) the decoder gives no more
        // output for new input, so a seek back from the end starts a fresh one.
        if self.drained {
            match Self::new(&self.init) {
                Ok(fresh) => {
                    *self = fresh;
                    return;
                }
                Err(e) => log::warn!("video: restarting the decoder: {e}"),
            }
        }
        // SAFETY: valid transform.
        let _ = unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) };
    }

    fn finish(&mut self) -> Result<Vec<(Duration, Picture)>> {
        // SAFETY: valid transform.
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
            .map_err(|e| mf_err("drain", e))?;
        self.drained = true;
        let mut out = Vec::new();
        self.drain_output(&mut out)?;
        Ok(out)
    }
}
