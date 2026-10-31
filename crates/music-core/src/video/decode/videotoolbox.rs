//! VideoToolbox: hardware H.264 decoding on every Mac. The session takes the
//! samples as stored (length-prefixed NAL units, `avcC` parameter sets) and
//! decodes on the calling thread (no asynchronous flag: the output callback
//! runs inside `decode_frame`), with temporal processing so frames come out
//! in display order, straight into IOSurface-backed `420f` buffers, the
//! format GPUI's Metal renderer draws surfaces from.

use std::ffi::c_void;
use std::ptr::{NonNull, null_mut};
use std::sync::Mutex;
use std::time::Duration;

use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, kCFAllocatorNull,
};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets, kCMTimeInvalid,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession,
};

use super::{Decoder, Picture};
use crate::video::mp4::{InitSegment, Sample};
use crate::{Error, Result};

/// Microsecond timestamps in and out of CoreMedia.
const TIMESCALE: i32 = 1_000_000;

type Outputs = Mutex<Vec<(Duration, CFRetained<CVPixelBuffer>)>>;

pub(crate) struct VtDecoder {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    /// Where the callback puts decoded frames (boxed: the session holds its address).
    outputs: Box<Outputs>,
}

// SAFETY: the session and format description are CoreFoundation objects used
// from one thread at a time (the decoder lives on the video worker).
unsafe impl Send for VtDecoder {}

fn vt_err(what: &str, status: i32) -> Error {
    Error::Unplayable(format!("VideoToolbox: {what} failed ({status})"))
}

unsafe extern "C-unwind" fn on_frame(
    refcon: *mut c_void,
    _frame: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    pts: CMTime,
    _duration: CMTime,
) {
    let Some(image) = NonNull::new(image) else {
        return;
    };
    if status != 0 || refcon.is_null() {
        return;
    }
    // SAFETY: `refcon` is the `Outputs` box the session was created with; it
    // outlives the session. The image buffer is borrowed: retain it.
    let outputs = unsafe { &*(refcon as *const Outputs) };
    let buffer = unsafe { CFRetained::retain(image) };
    let seconds = unsafe { pts.seconds() };
    let pts = if seconds.is_finite() && seconds >= 0.0 {
        Duration::from_secs_f64(seconds)
    } else {
        Duration::ZERO
    };
    if let Ok(mut out) = outputs.lock() {
        out.push((pts, buffer));
    }
}

impl VtDecoder {
    pub(crate) fn new(init: &InitSegment) -> Result<Self> {
        let sets: Vec<&Vec<u8>> = init.avc.sps.iter().chain(&init.avc.pps).collect();
        let mut pointers: Vec<NonNull<u8>> = sets
            .iter()
            .map(|s| NonNull::new(s.as_ptr().cast_mut()).unwrap_or(NonNull::dangling()))
            .collect();
        let mut sizes: Vec<usize> = sets.iter().map(|s| s.len()).collect();
        let mut format: *const CMFormatDescription = std::ptr::null();
        // SAFETY: the pointer and size arrays have one entry per parameter set and
        // live for the call; the output pointer is valid.
        let status = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                sets.len(),
                NonNull::new_unchecked(pointers.as_mut_ptr()),
                NonNull::new_unchecked(sizes.as_mut_ptr()),
                i32::from(init.avc.nal_length_size),
                NonNull::from(&mut format),
            )
        };
        let format = NonNull::new(format.cast_mut())
            .filter(|_| status == 0)
            .ok_or_else(|| vt_err("reading the parameter sets", status))?;
        // SAFETY: a +1 object from a Create function.
        let format = unsafe { CFRetained::from_raw(format) };

        let pixel_format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange as i32);
        let no_properties = CFDictionary::<CFString, CFType>::empty();
        // SAFETY: CoreVideo's key constants are valid CFStrings for the process.
        let (k_format, k_iosurface, k_metal) = unsafe {
            (
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferMetalCompatibilityKey,
            )
        };
        let attributes = CFDictionary::<CFString, CFType>::from_slices(
            &[k_format, k_iosurface, k_metal],
            &[
                pixel_format.as_ref(),
                no_properties.as_ref(),
                CFBoolean::new(true).as_ref(),
            ],
        );

        let outputs: Box<Outputs> = Box::default();
        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_frame),
            decompressionOutputRefCon: (&*outputs as *const Outputs).cast_mut().cast(),
        };
        let mut session: *mut VTDecompressionSession = null_mut();
        // SAFETY: valid format description and attributes; the callback record is
        // copied by the session; `outputs` stays at its address (boxed) and is
        // dropped after the session is invalidated.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                None,
                Some(attributes.as_opaque()),
                &callback,
                NonNull::from(&mut session),
            )
        };
        let session = NonNull::new(session)
            .filter(|_| status == 0)
            .ok_or_else(|| vt_err("creating the session", status))?;
        // SAFETY: a +1 object from a Create function.
        let session = unsafe { CFRetained::from_raw(session) };
        Ok(Self {
            session,
            format,
            outputs,
        })
    }
}

impl Decoder for VtDecoder {
    fn decode(&mut self, sample: &Sample) -> Result<Vec<(Duration, Picture)>> {
        let mut block: *mut CMBlockBuffer = null_mut();
        // SAFETY: the block points at `sample.data` without copying or freeing it
        // (kCFAllocatorNull); it is only used by the synchronous decode below.
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                sample.data.as_ptr().cast_mut().cast(),
                sample.data.len(),
                kCFAllocatorNull,
                std::ptr::null(),
                0,
                sample.data.len(),
                0,
                NonNull::from(&mut block),
            )
        };
        let block = NonNull::new(block)
            .filter(|_| status == 0)
            .ok_or_else(|| vt_err("wrapping a sample", status))?;
        // SAFETY: a +1 object from a Create function.
        let block = unsafe { CFRetained::from_raw(block) };
        let timing = CMSampleTimingInfo {
            // SAFETY: plain value constructors.
            duration: unsafe { CMTime::new(sample.duration.as_micros() as i64, TIMESCALE) },
            presentationTimeStamp: unsafe { CMTime::new(sample.pts.as_micros() as i64, TIMESCALE) },
            decodeTimeStamp: unsafe { kCMTimeInvalid },
        };
        let size = sample.data.len();
        let mut buffer: *mut CMSampleBuffer = null_mut();
        // SAFETY: one sample with one timing and one size entry; valid objects.
        let status = unsafe {
            CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(&self.format),
                1,
                1,
                &timing,
                1,
                &size,
                NonNull::from(&mut buffer),
            )
        };
        let buffer = NonNull::new(buffer)
            .filter(|_| status == 0)
            .ok_or_else(|| vt_err("making a sample buffer", status))?;
        // SAFETY: a +1 object from a Create function.
        let buffer = unsafe { CFRetained::from_raw(buffer) };
        let mut info = VTDecodeInfoFlags(0);
        // SAFETY: decoded on this thread (the callback has run for every frame
        // that is ready when this returns; display order may hold some back).
        let status = unsafe {
            self.session.decode_frame(
                &buffer,
                VTDecodeFrameFlags::Frame_EnableTemporalProcessing,
                null_mut(),
                &mut info,
            )
        };
        if status != 0 {
            return Err(vt_err("decoding a frame", status));
        }
        Ok(self.take())
    }

    fn flush(&mut self) {
        // SAFETY: valid session; emits the frames held for reordering, which
        // belong to the old position and are dropped.
        unsafe {
            self.session.finish_delayed_frames();
        }
        drop(self.take());
    }

    fn finish(&mut self) -> Result<Vec<(Duration, Picture)>> {
        // SAFETY: valid session.
        unsafe {
            self.session.finish_delayed_frames();
        }
        Ok(self.take())
    }
}

impl VtDecoder {
    fn take(&self) -> Vec<(Duration, Picture)> {
        let mut outputs = self.outputs.lock().unwrap_or_else(|e| e.into_inner());
        outputs
            .drain(..)
            .map(|(pts, buffer)| (pts, Picture::PixelBuffer(buffer)))
            .collect()
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        // SAFETY: the session is valid; after this no callback runs, so `outputs`
        // can go.
        unsafe { self.session.invalidate() };
    }
}
