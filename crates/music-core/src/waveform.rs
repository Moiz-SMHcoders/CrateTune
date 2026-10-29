//! The loudness shape of a whole track ("waveform"), for a SoundCloud-style
//! progress bar.
//!
//! [`compute`] reads the track on its own — a local file, bytes in memory, or
//! the [`StreamBuffer`] the player is already downloading into (reads block
//! until the bytes arrive; nothing is fetched twice) — and keeps only one
//! energy sum per 50 ms window. At the end the windows are reduced to
//! [`WAVEFORM_BUCKETS`] RMS values, scaled to decibels below the loudest bucket
//! and stored as one byte each: a [`Waveform`] is ~260 bytes.
//!
//! Cost: AAC, MP3, FLAC, … are fully decoded with Symphonia (a 4-minute AAC
//! song: ~0.15 s of CPU). Opus is *not* decoded: `opus-decoder` needs ~0.1 s of
//! CPU per second of audio, so the band energies are read straight from the
//! bitstream instead (`opus_energy`, ~1000× cheaper; [`compute_decoded`] does
//! the full decode for comparison).
//!
//! [`WaveformCache`] keeps finished waveforms on disk (one tiny file per track,
//! keyed by video id, or by path + modification time for local files), so a
//! replayed track costs a file read.
//!
//! The player runs all of this itself, one track at a time on one low-priority
//! thread, while enabled ([`crate::Player::set_waveform_enabled`],
//! [`crate::Player::waveform`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::buffer::{BufferEnd, StreamBuffer};
use crate::decode::{Demuxer, Input};
use crate::opus_energy::OpusEnergy;
use crate::{Error, Result, Track, TrackDecoder};

/// Buckets in a computed [`Waveform`] (one value per bucket).
pub const WAVEFORM_BUCKETS: usize = 256;
/// Energy windows per second while decoding (the resolution before bucketing).
const WINDOWS_PER_SEC: u32 = 20;
/// Buckets this far below the loudest one draw as 0.
const RANGE_DB: f32 = 30.0;
/// A track whose loudest bucket is below this RMS (−80 dBFS) is silence.
const SILENCE_RMS: f32 = 1e-4;
/// Cache files kept before the oldest are removed (each is a few hundred bytes).
const CACHE_MAX_FILES: usize = 5000;
const CACHE_MAGIC: &[u8; 4] = b"TDWF";
const CACHE_VERSION: u8 = 1;

/// The loudness envelope of a track: `peaks[i]` is the level of the i-th equal
/// slice of the track, 0 (≥ 30 dB below the loudest slice, or silence) to 255
/// (the loudest slice).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Waveform {
    pub peaks: Vec<u8>,
}

impl Waveform {
    pub fn len(&self) -> usize {
        self.peaks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peaks.is_empty()
    }

    /// Level 0..=1 at `frac` (0..=1) of the track.
    pub fn level_at(&self, frac: f32) -> f32 {
        if self.peaks.is_empty() {
            return 0.0;
        }
        let i =
            ((frac.clamp(0.0, 1.0) * self.peaks.len() as f32) as usize).min(self.peaks.len() - 1);
        f32::from(self.peaks[i]) / 255.0
    }

    /// The highest level 0..=1 over `from..to` (fractions of the track): for
    /// drawing fewer bars than there are buckets (or more).
    pub fn max_in(&self, from: f32, to: f32) -> f32 {
        let n = self.peaks.len();
        if n == 0 {
            return 0.0;
        }
        let a = ((from.clamp(0.0, 1.0) * n as f32) as usize).min(n - 1);
        let b = ((to.clamp(0.0, 1.0) * n as f32).ceil() as usize).clamp(a + 1, n);
        f32::from(self.peaks[a..b].iter().copied().max().unwrap_or(0)) / 255.0
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(7 + self.peaks.len());
        out.extend_from_slice(CACHE_MAGIC);
        out.push(CACHE_VERSION);
        out.extend_from_slice(&(self.peaks.len().min(u16::MAX as usize) as u16).to_le_bytes());
        out.extend_from_slice(&self.peaks[..self.peaks.len().min(u16::MAX as usize)]);
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let rest = bytes.strip_prefix(CACHE_MAGIC)?;
        let (&version, rest) = rest.split_first()?;
        if version != CACHE_VERSION || rest.len() < 2 {
            return None;
        }
        let n = usize::from(u16::from_le_bytes([rest[0], rest[1]]));
        let peaks = &rest[2..];
        (n > 0 && peaks.len() == n).then(|| Self {
            peaks: peaks.to_vec(),
        })
    }
}

/// Where [`compute`] reads the track from.
#[derive(Clone)]
pub enum WaveformSource {
    File(PathBuf),
    Memory(Arc<[u8]>),
    /// A download in progress (or finished); `mime_type` as the server sent it.
    Stream {
        buffer: Arc<StreamBuffer>,
        mime_type: String,
    },
}

/// What a [`compute`] run cost.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WaveformCost {
    /// CPU time of the computing thread (`None` where the OS can't say).
    pub cpu: Option<Duration>,
    /// Wall time, including waiting for downloaded bytes.
    pub wall: Duration,
    /// Length of the audio that was analyzed.
    pub audio: Duration,
    /// How: `"decode"` (full decode) or `"opus-bitstream"` (band energies
    /// read from the Opus frames).
    pub method: &'static str,
}

/// Sums the energy of interleaved audio per short window; [`Self::finish`]
/// reduces the windows to buckets. Memory: 4 bytes per 50 ms of audio.
pub(crate) struct Envelope {
    rate: u32,
    window: u64,
    sum: f64,
    frames: u64,
    total_frames: u64,
    /// Mean square per window.
    windows: Vec<f32>,
}

impl Envelope {
    pub(crate) fn new() -> Self {
        Self {
            rate: 0,
            window: 1,
            sum: 0.0,
            frames: 0,
            total_frames: 0,
            windows: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, samples: &[f32], channels: usize, rate: u32) {
        let channels = channels.max(1);
        if rate != self.rate && rate > 0 {
            self.rate = rate;
            self.window = u64::from((rate / WINDOWS_PER_SEC).max(1));
        }
        let scale = 1.0 / channels as f32;
        for frame in samples.chunks_exact(channels) {
            let energy: f32 = frame.iter().map(|s| s * s).sum::<f32>() * scale;
            self.sum += f64::from(energy);
            self.frames += 1;
            if self.frames >= self.window {
                self.flush();
            }
        }
        self.total_frames += (samples.len() / channels) as u64;
    }

    /// `frames` frames whose mean energy per sample is `energy`.
    pub(crate) fn push_constant(&mut self, energy: f32, mut frames: u64, rate: u32) {
        if rate != self.rate && rate > 0 {
            self.rate = rate;
            self.window = u64::from((rate / WINDOWS_PER_SEC).max(1));
        }
        self.total_frames += frames;
        while frames > 0 {
            let take = (self.window - self.frames.min(self.window)).clamp(1, frames);
            self.sum += f64::from(energy) * take as f64;
            self.frames += take;
            frames -= take;
            if self.frames >= self.window {
                self.flush();
            }
        }
    }

    fn flush(&mut self) {
        if self.frames > 0 {
            self.windows.push((self.sum / self.frames as f64) as f32);
        }
        self.sum = 0.0;
        self.frames = 0;
    }

    /// Seconds of audio pushed so far (at the last rate).
    pub(crate) fn duration(&self) -> Duration {
        if self.rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.total_frames as f64 / f64::from(self.rate))
    }

    /// `buckets` levels, or `None` when no audio was pushed.
    pub(crate) fn finish(mut self, buckets: usize) -> Option<Waveform> {
        // A last partial window counts if it holds a quarter window or more.
        if self.frames * 4 >= self.window || self.windows.is_empty() {
            self.flush();
        }
        let w = self.windows.len();
        if w == 0 || buckets == 0 {
            return None;
        }
        let rms: Vec<f32> = (0..buckets)
            .map(|b| {
                let start = b * w / buckets;
                let end = ((b + 1) * w / buckets).clamp(start + 1, w);
                let slice = &self.windows[start..end];
                (slice.iter().sum::<f32>() / slice.len() as f32).sqrt()
            })
            .collect();
        let max = rms.iter().copied().fold(0.0, f32::max);
        let peaks = rms
            .iter()
            .map(|&r| {
                if max < SILENCE_RMS || r <= 0.0 {
                    return 0;
                }
                let db = 20.0 * (r / max).log10();
                (((db + RANGE_DB) / RANGE_DB).clamp(0.0, 1.0) * 255.0).round() as u8
            })
            .collect();
        Some(Waveform { peaks })
    }
}

/// Reads `source` from start to end and returns its [`Waveform`] with
/// `buckets` values. Blocks (reading a stream waits for its download);
/// returns early with an error when `cancel` is set. A stream must download
/// completely: a preview, a failed or cancelled download is an error.
pub fn compute(
    source: &WaveformSource,
    buckets: usize,
    cancel: &AtomicBool,
) -> Result<(Waveform, WaveformCost)> {
    run(source, buckets, cancel, true)
}

/// [`compute`] with a full decode for every codec (Opus too; slow).
pub fn compute_decoded(
    source: &WaveformSource,
    buckets: usize,
    cancel: &AtomicBool,
) -> Result<(Waveform, WaveformCost)> {
    run(source, buckets, cancel, false)
}

fn cancelled() -> Error {
    Error::Audio("waveform cancelled".into())
}

fn run(
    source: &WaveformSource,
    buckets: usize,
    cancel: &AtomicBool,
    bitstream: bool,
) -> Result<(Waveform, WaveformCost)> {
    let started = Instant::now();
    let cpu_start = thread_cpu_time();
    let input = match source {
        WaveformSource::File(path) => Input::File(path.clone()),
        WaveformSource::Memory(bytes) => Input::Memory(bytes.clone()),
        WaveformSource::Stream { buffer, mime_type } => {
            if buffer.total_len().is_none() {
                return Err(Error::Audio("stream of unknown length".into()));
            }
            Input::stream(buffer.clone(), mime_type)
        }
    };
    let mut method = "decode";
    let mut envelope = None;
    if bitstream {
        let mut demuxer = Demuxer::open(&input)?;
        if let Some(channels) = demuxer.plain_opus_channels() {
            envelope = opus_envelope(&mut demuxer, channels, cancel)?;
            method = "opus-bitstream";
        }
    }
    let envelope = match envelope {
        Some(envelope) => envelope,
        None => {
            method = "decode";
            decoded_envelope(TrackDecoder::open(input)?, cancel)?
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(cancelled());
    }
    if let WaveformSource::Stream { buffer, .. } = source {
        match buffer.end() {
            Some(BufferEnd::Complete) => {}
            Some(BufferEnd::Truncated) => {
                return Err(Error::Audio("only a preview of the stream".into()));
            }
            Some(BufferEnd::Cancelled) => return Err(cancelled()),
            Some(BufferEnd::Failed(e)) => return Err(Error::Audio(e)),
            None => return Err(Error::Audio("stream ended early".into())),
        }
    }
    let audio = envelope.duration();
    let waveform = envelope
        .finish(buckets)
        .ok_or_else(|| Error::Audio("no audio".into()))?;
    let cost = WaveformCost {
        cpu: cpu_start
            .zip(thread_cpu_time())
            .map(|(a, b)| b.saturating_sub(a)),
        wall: started.elapsed(),
        audio,
        method,
    };
    Ok((waveform, cost))
}

/// Full decode, one packet at a time.
fn decoded_envelope(mut decoder: TrackDecoder, cancel: &AtomicBool) -> Result<Envelope> {
    let mut envelope = Envelope::new();
    loop {
        // One atomic load per packet (~20 ms of audio).
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let Some((block, rate, channels)) = decoder.next_block() else {
            break;
        };
        envelope.push(block, channels, rate);
    }
    if let Some(e) = decoder.end_error() {
        return Err(Error::Audio(format!("waveform: {e}")));
    }
    Ok(envelope)
}

/// Opus band energies from the bitstream; `None` when too much of the track
/// isn't CELT (SILK / hybrid frames, i.e. speech), so the caller decodes.
fn opus_envelope(
    demuxer: &mut Demuxer,
    channels: usize,
    cancel: &AtomicBool,
) -> Result<Option<Envelope>> {
    let mut energy = OpusEnergy::new(channels);
    let mut envelope = Envelope::new();
    let (mut celt, mut other) = (0u64, 0u64);
    let mut last = 0.0;
    while let Some(packet) = demuxer.next_packet()? {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let Some(p) = energy.packet(&packet) else {
            continue; // malformed: skip it
        };
        let e = match p.energy {
            Some(e) => {
                celt += u64::from(p.frames);
                last = e;
                e
            }
            None => {
                other += u64::from(p.frames);
                last
            }
        };
        envelope.push_constant(e, u64::from(p.frames), 48_000);
    }
    Ok((other * 10 <= celt + other).then_some(envelope))
}

/// CPU time used by the calling thread so far.
fn thread_cpu_time() -> Option<Duration> {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, writable timespec.
        let ok = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0;
        ok.then(|| {
            Duration::new(
                ts.tv_sec.max(0) as u64,
                ts.tv_nsec.clamp(0, 999_999_999) as u32,
            )
        })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Moves the calling thread to the background: QoS "background" on macOS
/// (efficiency cores, throttled I/O), `nice` 19 on Linux. Elsewhere a no-op.
pub(crate) fn lower_thread_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: plain syscall on the calling thread; failure leaves the priority as is.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
    }
    #[cfg(target_os = "linux")]
    // SAFETY: plain syscalls on the calling thread; failure leaves the priority as is.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 19);
    }
}

/// The cache key of `track`'s waveform: its video id, or for a local file
/// (`local:` ids) the path plus its modification time and size, so an edited
/// file gets a new waveform. `None` when neither is usable. Stats the file.
pub fn cache_key(track: &Track) -> Option<String> {
    let id = track.video_id.as_str();
    let plain_id = !id.is_empty()
        && id.len() <= 64
        && !id.starts_with("local:")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if plain_id {
        return Some(format!("v-{id}"));
    }
    let path = track.local_path.as_deref()?;
    file_key(path)
}

fn file_key(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .unwrap_or_default();
    let text = format!(
        "{}\0{}.{:09}\0{}",
        path.to_string_lossy(),
        mtime.as_secs(),
        mtime.subsec_nanos(),
        meta.len()
    );
    Some(format!("f-{:x}", md5::compute(text.as_bytes())))
}

/// Finished waveforms on disk: one file per track (`<key>.wf`, a few hundred
/// bytes) in one directory; the oldest go once there are more than 5000.
pub struct WaveformCache {
    dir: PathBuf,
    puts: AtomicU32,
}

impl WaveformCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            puts: AtomicU32::new(0),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.wf"))
    }

    pub fn get(&self, key: &str) -> Option<Waveform> {
        Waveform::decode(&fs::read(self.path(key)).ok()?)
    }

    /// Writes atomically (temp file + rename).
    pub fn put(&self, key: &str, waveform: &Waveform) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(key);
        let tmp = path.with_extension("wf.tmp");
        fs::write(&tmp, waveform.encode())?;
        fs::rename(&tmp, &path)?;
        // Look at the directory on the first write of a session and every 100th.
        if self
            .puts
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(100)
        {
            self.prune(CACHE_MAX_FILES);
        }
        Ok(())
    }

    /// Removes the least recently written files beyond `keep`.
    fn prune(&self, keep: usize) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "wf"))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        if files.len() <= keep {
            return;
        }
        files.sort();
        let excess = files.len() - keep;
        for (_, path) in files.into_iter().take(excess) {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16-bit stereo PCM WAV: `silent` seconds of silence, then `loud` of a sine.
    fn wav(rate: u32, silent: f32, loud: f32) -> Vec<u8> {
        let frames = ((silent + loud) * rate as f32) as usize;
        let quiet = (silent * rate as f32) as usize;
        let mut pcm = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            let s = if i < quiet {
                0
            } else {
                let t = i as f32 / rate as f32;
                ((t * 440.0 * std::f32::consts::TAU).sin() * 0.8 * i16::MAX as f32) as i16
            };
            pcm.extend_from_slice(&s.to_le_bytes());
            pcm.extend_from_slice(&s.to_le_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&2u16.to_le_bytes()); // channels
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(&pcm);
        out
    }

    #[test]
    fn silence_then_loud_gives_low_then_high_buckets() {
        let bytes: Arc<[u8]> = wav(8000, 4.0, 4.0).into();
        let cancel = AtomicBool::new(false);
        let (wf, cost) =
            compute(&WaveformSource::Memory(bytes), WAVEFORM_BUCKETS, &cancel).unwrap();
        assert_eq!(wf.len(), WAVEFORM_BUCKETS);
        assert!((cost.audio.as_secs_f32() - 8.0).abs() < 0.1, "{cost:?}");
        let half = WAVEFORM_BUCKETS / 2;
        // First half silent, second half at full level (allow one edge bucket).
        assert!(
            wf.peaks[..half - 1].iter().all(|&p| p == 0),
            "{:?}",
            wf.peaks
        );
        assert!(
            wf.peaks[half + 1..].iter().all(|&p| p >= 250),
            "{:?}",
            wf.peaks
        );
        assert_eq!(wf.level_at(0.1), 0.0);
        assert!(wf.level_at(0.9) > 0.98);
        assert!(wf.max_in(0.45, 0.55) > 0.98);
        assert_eq!(wf.max_in(0.0, 0.4), 0.0);
    }

    #[test]
    fn levels_are_decibels_below_the_loudest_bucket() {
        let mut env = Envelope::new();
        // One second at amplitude 1, one at 0.5 (−6 dB), one at 0.01 (−40 dB).
        for amp in [1.0f32, 0.5, 0.01] {
            let block: Vec<f32> = (0..1000)
                .map(|i| if i % 2 == 0 { amp } else { -amp })
                .collect();
            env.push(&block, 1, 1000);
        }
        let wf = env.finish(3).unwrap();
        assert_eq!(wf.peaks[0], 255);
        let expected = ((RANGE_DB - 6.02) / RANGE_DB * 255.0).round() as i32;
        assert!(
            (i32::from(wf.peaks[1]) - expected).abs() <= 1,
            "{:?}",
            wf.peaks
        );
        assert_eq!(wf.peaks[2], 0);
    }

    #[test]
    fn bucket_count_is_fixed_for_short_and_long_input() {
        for (seconds, buckets) in [(0.3f32, 256usize), (2.0, 256), (30.0, 256), (5.0, 7)] {
            let mut env = Envelope::new();
            let frames = (seconds * 8000.0) as usize;
            env.push(&vec![0.25; frames * 2], 2, 8000);
            let wf = env.finish(buckets).unwrap();
            assert_eq!(wf.len(), buckets, "{seconds} s");
            assert!(
                wf.peaks.iter().all(|&p| p == 255),
                "a constant level is all max"
            );
        }
        assert!(Envelope::new().finish(256).is_none());
        // Pure silence is all zeros, not noise blown up to full height.
        let mut env = Envelope::new();
        env.push(&[0.0; 16000], 2, 8000);
        assert!(env.finish(256).unwrap().peaks.iter().all(|&p| p == 0));
    }

    #[test]
    fn cache_round_trip_and_keys() {
        let dir = std::env::temp_dir().join(format!(
            "music-core-waveform-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let cache = WaveformCache::new(&dir);
        let wf = Waveform {
            peaks: (0..=255).collect(),
        };
        assert_eq!(cache.get("v-abc"), None);
        cache.put("v-abc", &wf).unwrap();
        assert_eq!(cache.get("v-abc"), Some(wf.clone()));
        let size = fs::metadata(dir.join("v-abc.wf")).unwrap().len();
        assert_eq!(size, 7 + 256, "a few hundred bytes per track");
        // Garbage and other versions are ignored.
        fs::write(dir.join("v-bad.wf"), b"TDWF\x09\x01\x00\x07").unwrap();
        assert_eq!(cache.get("v-bad"), None);

        // Keys: video ids as they are; local files by path + mtime + size.
        let track = Track {
            video_id: "kJQP7kiw5Fk".into(),
            ..Default::default()
        };
        assert_eq!(cache_key(&track).as_deref(), Some("v-kJQP7kiw5Fk"));
        let file = dir.join("song.wav");
        fs::write(&file, b"one").unwrap();
        let local = Track {
            video_id: "local:0123".into(),
            local_path: Some(file.clone()),
            ..Default::default()
        };
        let key = cache_key(&local).unwrap();
        assert!(key.starts_with("f-"));
        fs::write(&file, b"longer").unwrap();
        assert_ne!(
            cache_key(&local).unwrap(),
            key,
            "an edited file gets a new key"
        );
        assert_eq!(
            cache_key(&Track {
                video_id: "local:gone".into(),
                ..Default::default()
            }),
            None
        );

        // Pruning keeps the newest files.
        cache.prune(1);
        let left = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "wf"))
            .count();
        assert_eq!(left, 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancel_stops_a_stream_waiting_for_bytes() {
        // A stream whose download stalls after the header: compute blocks in a
        // read until the buffer is cancelled (what the player does on a track
        // change), then returns promptly.
        let bytes = wav(8000, 1.0, 9.0);
        let buffer = StreamBuffer::new(Some(bytes.len() as u64));
        buffer.append(&bytes[..bytes.len() / 4]);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = std::thread::spawn({
            let (buffer, cancel) = (buffer.clone(), cancel.clone());
            move || {
                compute(
                    &WaveformSource::Stream {
                        buffer,
                        mime_type: "audio/wav".into(),
                    },
                    WAVEFORM_BUCKETS,
                    &cancel,
                )
            }
        });
        std::thread::sleep(Duration::from_millis(100));
        let t = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        buffer.cancel();
        assert!(worker.join().unwrap().is_err());
        assert!(t.elapsed() < Duration::from_secs(1));

        // The flag alone stops a decode that has all its bytes.
        let cancel = AtomicBool::new(true);
        let all: Arc<[u8]> = bytes.into();
        assert!(compute(&WaveformSource::Memory(all), 64, &cancel).is_err());
    }

    #[test]
    fn opus_is_read_from_the_bitstream_and_agrees_with_a_decode() {
        // 2 s of silence, then 2 s of a sine; CELT-only (libopus, lowdelay).
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/quiet_loud_celt.webm");
        let source = WaveformSource::File(path);
        let no = AtomicBool::new(false);
        let (fast, cost) = compute(&source, 64, &no).unwrap();
        assert_eq!(cost.method, "opus-bitstream");
        assert!((cost.audio.as_secs_f32() - 4.0).abs() < 0.1, "{cost:?}");
        assert!(fast.peaks[..30].iter().all(|&p| p < 20), "{:?}", fast.peaks);
        assert!(
            fast.peaks[34..].iter().all(|&p| p > 200),
            "{:?}",
            fast.peaks
        );
        let (slow, cost) = compute_decoded(&source, 64, &no).unwrap();
        assert_eq!(cost.method, "decode");
        let mean_diff = fast
            .peaks
            .iter()
            .zip(&slow.peaks)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .sum::<i32>()
            / 64;
        assert!(mean_diff < 25, "{:?}\n{:?}", fast.peaks, slow.peaks);
        // Mostly SILK / hybrid frames (a low-bitrate encode): decoded instead.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sine.opus");
        let (_, cost) = compute(&WaveformSource::File(path), 64, &no).unwrap();
        assert_eq!(cost.method, "decode");
    }

    #[test]
    fn stream_must_be_complete() {
        let bytes = wav(8000, 0.0, 4.0);
        let buffer = StreamBuffer::new(Some(bytes.len() as u64));
        buffer.append(&bytes[..bytes.len() / 2]);
        buffer.finish(BufferEnd::Truncated);
        let source = WaveformSource::Stream {
            buffer,
            mime_type: "audio/wav".into(),
        };
        assert!(compute(&source, 64, &AtomicBool::new(false)).is_err());
        let full = StreamBuffer::from_bytes(bytes);
        let source = WaveformSource::Stream {
            buffer: full,
            mime_type: "audio/wav".into(),
        };
        let (wf, _) = compute(&source, 64, &AtomicBool::new(false)).unwrap();
        assert_eq!(wf.len(), 64);
    }
}
