//! OpenH264 (Cisco, BSD-2-Clause, compiled from source by the `openh264`
//! crate): software H.264 decoding on Linux, where no decoder is part of the
//! system. It takes Annex B (start codes), so every keyframe gets the SPS /
//! PPS in front, and returns pictures in display order without their times:
//! those are the input times in ascending order.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::Duration;

use openh264::OpenH264API;
use openh264::decoder::{Decoder as OpenH264, DecoderConfig, Flush};
use openh264::formats::YUVSource;

use super::{Decoder, Picture, Yuv};
use crate::video::mp4::{InitSegment, Sample};
use crate::{Error, Result};

/// Input times kept for pictures still inside the decoder (B-frame reordering
/// holds back a few; more means some were dropped).
const MAX_PENDING: usize = 16;

pub(crate) struct OhDecoder {
    decoder: OpenH264,
    init: InitSegment,
    pending: BinaryHeap<Reverse<Duration>>,
}

impl OhDecoder {
    pub(crate) fn new(init: &InitSegment) -> Result<Self> {
        Ok(Self {
            decoder: open()?,
            init: init.clone(),
            pending: BinaryHeap::new(),
        })
    }
}

/// `yuv` as BGRA at the track's visible size, with the earliest time pending.
fn picture(
    yuv: &impl YUVSource,
    init: &InitSegment,
    pending: &mut BinaryHeap<Reverse<Duration>>,
) -> Option<(Duration, Picture)> {
    let pts = pending.pop()?.0;
    let (width, height) = yuv.dimensions();
    let (width, height) = (
        width.min(init.width as usize),
        height.min(init.height as usize),
    );
    let (y_stride, uv_stride, _) = yuv.strides();
    let data = Yuv {
        width,
        height,
        y: yuv.y(),
        y_stride,
        u: yuv.u(),
        v: yuv.v(),
        uv_stride,
        interleaved: false,
    }
    .to_bgra();
    Some((
        pts,
        Picture::Bgra {
            width: width as u32,
            height: height as u32,
            data,
        },
    ))
}

fn open() -> Result<OpenH264> {
    let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
    OpenH264::with_api_config(OpenH264API::from_source(), config)
        .map_err(|e| Error::Unplayable(format!("OpenH264: {e}")))
}

impl Decoder for OhDecoder {
    fn decode(&mut self, sample: &Sample) -> Result<Vec<(Duration, Picture)>> {
        let mut packet = Vec::with_capacity(sample.data.len() + 64);
        if sample.keyframe {
            packet.extend(self.init.avc.annex_b_parameter_sets());
        }
        packet.extend(
            self.init
                .avc
                .to_annex_b(&sample.data)
                .ok_or_else(|| Error::Parse("video: broken NAL unit lengths".into()))?,
        );
        self.pending.push(Reverse(sample.pts));
        while self.pending.len() > MAX_PENDING {
            self.pending.pop();
        }
        let Self {
            decoder,
            init,
            pending,
        } = self;
        match decoder.decode(&packet) {
            Ok(Some(yuv)) => Ok(picture(&yuv, init, pending).into_iter().collect()),
            Ok(None) => Ok(Vec::new()),
            // A broken frame: skip it, the next keyframe recovers.
            Err(e) => {
                log::debug!("video: OpenH264: {e}");
                Ok(Vec::new())
            }
        }
    }

    fn flush(&mut self) {
        // Pictures held for reordering belong to the old position: start over.
        match open() {
            Ok(decoder) => self.decoder = decoder,
            Err(e) => log::warn!("video: {e}"),
        }
        self.pending.clear();
    }

    fn finish(&mut self) -> Result<Vec<(Duration, Picture)>> {
        let Self {
            decoder,
            init,
            pending,
        } = self;
        match decoder.flush_remaining() {
            Ok(frames) => Ok(frames
                .iter()
                .filter_map(|yuv| picture(yuv, init, pending))
                .collect()),
            Err(e) => {
                log::debug!("video: OpenH264 flush: {e}");
                Ok(Vec::new())
            }
        }
    }
}
