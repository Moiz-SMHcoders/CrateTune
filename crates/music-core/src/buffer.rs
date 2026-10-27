//! A growing in-memory buffer for progressive downloads.
//!
//! A download thread [`StreamBuffer::append`]s bytes as they arrive; any number
//! of [`StreamReader`]s read from it concurrently through `Read + Seek` (and
//! Symphonia's `MediaSource`). A read past the downloaded part blocks until the
//! bytes arrive, the download ends, the buffer is cancelled, or the read timeout
//! passes; so a decoder can start as soon as the container header is in and keep
//! up with the download from there.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Default for [`StreamBuffer::set_read_timeout`].
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How a download ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferEnd {
    /// Every byte of the file arrived.
    Complete,
    /// The server stopped serving partway (e.g. googlevideo's ~1 MiB preview
    /// limit); the data is a valid prefix of the file.
    Truncated,
    /// A network or HTTP error ended the download.
    Failed(String),
    /// [`StreamBuffer::cancel`] was called.
    Cancelled,
}

/// Bytes of one file, filled by a downloader and read by any number of readers.
pub struct StreamBuffer {
    state: Mutex<BufState>,
    grew: Condvar,
    /// Read timeout in milliseconds.
    timeout_ms: AtomicU64,
}

struct BufState {
    data: Vec<u8>,
    total_len: Option<u64>,
    end: Option<BufferEnd>,
}

impl std::fmt::Debug for StreamBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("StreamBuffer")
            .field("len", &state.data.len())
            .field("total_len", &state.total_len)
            .field("end", &state.end)
            .finish()
    }
}

impl StreamBuffer {
    /// An empty buffer for a file of `total_len` bytes (if known).
    pub fn new(total_len: Option<u64>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BufState {
                data: Vec::with_capacity(total_len.unwrap_or(0).min(64 << 20) as usize),
                total_len,
                end: None,
            }),
            grew: Condvar::new(),
            timeout_ms: AtomicU64::new(DEFAULT_READ_TIMEOUT.as_millis() as u64),
        })
    }

    /// A finished buffer holding `bytes`.
    pub fn from_bytes(bytes: Vec<u8>) -> Arc<Self> {
        let len = bytes.len() as u64;
        let this = Self::new(Some(len));
        this.lock().data = bytes;
        this.finish(BufferEnd::Complete);
        this
    }

    fn lock(&self) -> MutexGuard<'_, BufState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Appends downloaded bytes and wakes blocked readers. Ignored once ended.
    pub fn append(&self, bytes: &[u8]) {
        let mut state = self.lock();
        if state.end.is_none() {
            state.data.extend_from_slice(bytes);
            drop(state);
            self.grew.notify_all();
        }
    }

    /// Marks the download as over. Only the first call has an effect.
    pub fn finish(&self, end: BufferEnd) {
        let mut state = self.lock();
        if state.end.is_none() {
            state.end = Some(end);
            drop(state);
            self.grew.notify_all();
        }
    }

    /// Stops the download (the downloader checks this between reads) and makes
    /// blocked and future reads past the downloaded part fail.
    pub fn cancel(&self) {
        self.finish(BufferEnd::Cancelled);
    }

    pub fn is_cancelled(&self) -> bool {
        self.lock().end == Some(BufferEnd::Cancelled)
    }

    /// Bytes downloaded so far.
    pub fn len(&self) -> u64 {
        self.lock().data.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Size of the whole file, if the server said so.
    pub fn total_len(&self) -> Option<u64> {
        self.lock().total_len
    }

    /// `None` while the download is still running.
    pub fn end(&self) -> Option<BufferEnd> {
        self.lock().end.clone()
    }

    /// How long a read past the downloaded part may block before failing
    /// with [`io::ErrorKind::TimedOut`].
    pub fn set_read_timeout(&self, timeout: Duration) {
        self.timeout_ms
            .store(timeout.as_millis() as u64, Ordering::Relaxed);
    }

    pub fn read_timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.load(Ordering::Relaxed))
    }

    /// Blocks until at least `len` bytes are in, the download ended, or
    /// `timeout` passed. Returns whether `len` bytes are available.
    pub fn wait_for(&self, len: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if state.data.len() as u64 >= len {
                return true;
            }
            let now = Instant::now();
            if state.end.is_some() || now >= deadline {
                return false;
            }
            state = self
                .grew
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Copy of the bytes downloaded so far.
    pub fn to_vec(&self) -> Vec<u8> {
        self.lock().data.clone()
    }

    /// A reader starting at byte 0.
    pub fn reader(self: &Arc<Self>) -> StreamReader {
        StreamReader {
            buffer: self.clone(),
            pos: 0,
        }
    }
}

/// Blocking `Read + Seek` view of a [`StreamBuffer`].
pub struct StreamReader {
    buffer: Arc<StreamBuffer>,
    pos: u64,
}

impl StreamReader {
    pub fn buffer(&self) -> &Arc<StreamBuffer> {
        &self.buffer
    }

    /// Length used for `SeekFrom::End`: the downloaded size of a complete
    /// download, else the size the server announced. A truncated preview keeps
    /// reporting the announced size, so a demuxer sees the same file whether
    /// it was opened before or after the cut (the MP4 reader plays the
    /// downloaded part of a cut-off fragment only when it believes the
    /// fragment is complete).
    fn known_len(state: &BufState) -> Option<u64> {
        match &state.end {
            Some(BufferEnd::Complete) => Some(state.data.len() as u64),
            Some(BufferEnd::Truncated) => state.total_len.or(Some(state.data.len() as u64)),
            _ => state.total_len,
        }
    }
}

impl Read for StreamReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let buffer = &self.buffer;
        let deadline = Instant::now() + buffer.read_timeout();
        let mut state = buffer.lock();
        loop {
            let len = state.data.len() as u64;
            if self.pos < len {
                let start = self.pos as usize;
                let n = out.len().min(len as usize - start);
                out[..n].copy_from_slice(&state.data[start..start + n]);
                self.pos += n as u64;
                return Ok(n);
            }
            if state.total_len.is_some_and(|total| self.pos >= total) {
                return Ok(0);
            }
            match &state.end {
                Some(BufferEnd::Complete | BufferEnd::Truncated) => return Ok(0),
                Some(BufferEnd::Failed(msg)) => return Err(io::Error::other(msg.clone())),
                Some(BufferEnd::Cancelled) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "download cancelled",
                    ));
                }
                None => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for audio data",
                ));
            }
            state = buffer
                .grew
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl Seek for StreamReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => {
                let state = self.buffer.lock();
                let len = Self::known_len(&state).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "stream length unknown")
                })?;
                len.checked_add_signed(d)
            }
        };
        self.pos = target
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.pos)
    }
}

impl symphonia::core::io::MediaSource for StreamReader {
    fn is_seekable(&self) -> bool {
        Self::known_len(&self.buffer.lock()).is_some()
    }

    /// Only known once the whole file is in: Symphonia's probe reads trailing
    /// tags (ID3v1/APE) at `byte_len - n` when it is known, which would block
    /// until the download finished. `Seek` still resolves `SeekFrom::End`
    /// from the server-reported size, so demuxers that need it keep working.
    fn byte_len(&self) -> Option<u64> {
        let state = self.buffer.lock();
        (state.end == Some(BufferEnd::Complete)).then_some(state.data.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn reads_block_until_bytes_arrive() {
        let buf = StreamBuffer::new(Some(8));
        let mut reader = buf.reader();
        let writer = thread::spawn({
            let buf = buf.clone();
            move || {
                for chunk in [b"abc".as_slice(), b"defgh"] {
                    thread::sleep(Duration::from_millis(30));
                    buf.append(chunk);
                }
                buf.finish(BufferEnd::Complete);
            }
        });
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abcdefgh");
        writer.join().unwrap();
    }

    #[test]
    fn seek_then_read_waits_for_the_target() {
        let buf = StreamBuffer::new(Some(6));
        buf.append(b"ab");
        let mut reader = buf.reader();
        assert_eq!(reader.seek(SeekFrom::End(-2)).unwrap(), 4);
        let t = thread::spawn({
            let buf = buf.clone();
            move || {
                thread::sleep(Duration::from_millis(20));
                buf.append(b"cdef");
            }
        });
        let mut two = [0u8; 2];
        reader.read_exact(&mut two).unwrap();
        assert_eq!(&two, b"ef");
        assert_eq!(reader.read(&mut two).unwrap(), 0, "EOF at total length");
        t.join().unwrap();
    }

    #[test]
    fn truncated_download_ends_reads_at_the_cut() {
        let buf = StreamBuffer::new(Some(100));
        buf.append(b"hello");
        let mut reader = buf.reader();
        buf.finish(BufferEnd::Truncated);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"hello");
        // Seeking past the cut is allowed; reading there is EOF. The length
        // stays the announced one.
        use symphonia::core::io::MediaSource;
        assert_eq!(reader.byte_len(), None);
        assert_eq!(reader.seek(SeekFrom::End(0)).unwrap(), 100);
        reader.seek(SeekFrom::Start(50)).unwrap();
        assert_eq!(reader.read(&mut [0u8; 4]).unwrap(), 0);
    }

    #[test]
    fn cancel_wakes_blocked_readers_with_an_error() {
        let buf = StreamBuffer::new(None);
        let mut reader = buf.reader();
        let t = thread::spawn({
            let buf = buf.clone();
            move || {
                thread::sleep(Duration::from_millis(20));
                buf.cancel();
            }
        });
        let err = reader.read(&mut [0u8; 4]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
        assert!(buf.is_cancelled());
        t.join().unwrap();
    }

    #[test]
    fn read_past_downloaded_data_times_out() {
        let buf = StreamBuffer::new(Some(10));
        buf.append(b"abc");
        buf.set_read_timeout(Duration::from_millis(30));
        let mut reader = buf.reader();
        reader.seek(SeekFrom::Start(5)).unwrap();
        let started = Instant::now();
        let err = reader.read(&mut [0u8; 1]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(30));
        // Data before the gap is still readable.
        reader.seek(SeekFrom::Start(1)).unwrap();
        let mut b = [0u8; 2];
        reader.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"bc");
    }

    #[test]
    fn failed_download_surfaces_as_io_error_after_the_data() {
        let buf = StreamBuffer::new(None);
        buf.append(b"xy");
        buf.finish(BufferEnd::Failed("boom".into()));
        buf.append(b"ignored");
        let mut reader = buf.reader();
        let mut b = [0u8; 2];
        reader.read_exact(&mut b).unwrap();
        assert!(reader.read(&mut b).is_err());
        assert_eq!(buf.len(), 2);
        assert!(buf.wait_for(2, Duration::ZERO));
        assert!(
            !buf.wait_for(3, Duration::from_secs(5)),
            "ended, so no wait"
        );
    }
}
