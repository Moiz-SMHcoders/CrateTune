//! User downloads and the automatic play cache, as audio files on disk.
//!
//! [`DownloadManager`] resolves tracks through the same [`StreamResolver`] the player uses
//! and fetches them with [`ResolvedStream::download`] on two worker threads. Without a
//! PoToken that yields only the ~1 MiB preview, recorded as `complete: false`.
//!
//! Two kinds of files live side by side (cf. ArchiveTune's download cache vs. player cache):
//!
//! * [`DownloadKind::Download`] — the user asked for it; never evicted.
//! * [`DownloadKind::Cache`] — automatic copies of played songs, evicted least recently used
//!   first once the cache exceeds its size limit ([`DownloadManager::set_cache_limit`]).
//!
//! The UI polls [`DownloadManager::states`] like it polls `Player::state`. Records are kept
//! in `index.json` in the download directory; with a [`Library`], user downloads are also
//! reflected on the song (`downloaded_at`, [`crate::library::SongScope::Downloaded`]).
//!
//! Cancelling a running download can't interrupt [`ResolvedStream::download`]; the job is
//! marked cancelled at once and its data dropped when the fetch returns.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

use serde::{Deserialize, Serialize};

use crate::library::now_ms;
use crate::{Error, Library, Result, StreamResolver, Track};

/// Worker threads (parallel downloads).
pub const CONCURRENCY: usize = 2;
/// Default limit for [`DownloadKind::Cache`] files.
pub const DEFAULT_CACHE_LIMIT: u64 = 512 * 1024 * 1024;
/// Finished jobs kept in [`DownloadManager::states`] until [`DownloadManager::clear_finished`].
const MAX_FINISHED_JOBS: usize = 200;
const INDEX_FILE: &str = "index.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DownloadKind {
    /// Requested by the user; never evicted.
    Download,
    /// Automatic cache of played songs; LRU-evicted.
    Cache,
}

impl DownloadKind {
    fn dir_name(self) -> &'static str {
        match self {
            DownloadKind::Download => "downloads",
            DownloadKind::Cache => "cache",
        }
    }
}

/// A stored audio file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub track: Track,
    pub kind: DownloadKind,
    /// Absolute path of the audio file.
    pub path: PathBuf,
    pub size: u64,
    /// False when only the start of the track could be fetched (preview).
    pub complete: bool,
    pub mime_type: String,
    pub loudness_db: Option<f32>,
    pub created_at: i64,
    pub last_access_at: i64,
    /// Monotonic access counter (LRU order within one millisecond).
    #[serde(default)]
    access_seq: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    Queued,
    /// Resolving and fetching.
    Downloading,
    /// Stored; see [`DownloadState::complete`].
    Completed,
    Failed(String),
    Cancelled,
}

impl DownloadStatus {
    pub fn is_finished(&self) -> bool {
        !matches!(self, DownloadStatus::Queued | DownloadStatus::Downloading)
    }
}

/// Snapshot of one job, cheap to clone.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadState {
    pub track: Track,
    pub kind: DownloadKind,
    pub status: DownloadStatus,
    /// Expected size from the stream metadata, once resolved.
    pub total_bytes: Option<u64>,
    /// Bytes stored, once completed.
    pub size: u64,
    /// Whether the whole track was fetched (false = preview only).
    pub complete: bool,
}

struct Job {
    id: u64,
    state: DownloadState,
    cancelled: bool,
}

struct State {
    records: HashMap<String, DownloadRecord>,
    jobs: Vec<Job>,
    queue: VecDeque<u64>,
    next_job: u64,
    access_seq: u64,
    cache_limit: u64,
    shutdown: bool,
}

struct Inner {
    dir: PathBuf,
    resolver: Arc<dyn StreamResolver>,
    library: Option<Library>,
    state: Mutex<State>,
    wake: Condvar,
}

/// Download queue + file store. Dropping it stops the workers after their current job.
pub struct DownloadManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for DownloadManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadManager")
            .field("dir", &self.inner.dir)
            .finish_non_exhaustive()
    }
}

fn file_name(video_id: &str, mime_type: &str) -> String {
    let safe: String = video_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Distinct ids that sanitize alike (e.g. `local:x` vs `local_x`) get distinct files.
    let safe = if safe == video_id {
        safe
    } else {
        format!("{safe}-{:x}", md5::compute(video_id))
    };
    let ext = match mime_type.split(';').next().unwrap_or_default().trim() {
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/webm" => "webm",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        _ => "bin",
    };
    format!("{safe}.{ext}")
}

/// Writes `bytes` to `path` via a temp file + rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save_index(&self, state: &State) {
        let mut records: Vec<&DownloadRecord> = state.records.values().collect();
        records.sort_by(|a, b| a.track.video_id.cmp(&b.track.video_id));
        let result = serde_json::to_vec(&records)
            .map_err(|e| Error::Storage(e.to_string()))
            .and_then(|json| write_atomic(&self.dir.join(INDEX_FILE), &json));
        if let Err(e) = result {
            log::warn!("downloads: saving index failed: {e}");
        }
    }

    fn mark_library(&self, track: &Track, complete: Option<bool>) {
        if let Some(library) = &self.library
            && let Err(e) = library.set_downloaded(track, complete)
        {
            log::warn!("downloads: library update failed: {e}");
        }
    }

    /// Removes cache files until the cache fits its limit. Returns evicted ids.
    fn evict(&self, state: &mut State) -> Vec<String> {
        let mut cached: Vec<(u64, u64, String)> = state
            .records
            .values()
            .filter(|r| r.kind == DownloadKind::Cache)
            .map(|r| (r.access_seq, r.size, r.track.video_id.clone()))
            .collect();
        let mut total: u64 = cached.iter().map(|c| c.1).sum();
        cached.sort();
        let mut evicted = Vec::new();
        for (_, size, id) in cached {
            if total <= state.cache_limit {
                break;
            }
            if let Some(record) = state.records.remove(&id) {
                let _ = std::fs::remove_file(&record.path);
                total -= size;
                evicted.push(id);
            }
        }
        evicted
    }

    /// Stores fetched audio and returns the record. Caller holds no lock.
    fn store(
        &self,
        track: &Track,
        kind: DownloadKind,
        bytes: &[u8],
        complete: bool,
        mime_type: &str,
        loudness_db: Option<f32>,
    ) -> Result<DownloadRecord> {
        if kind == DownloadKind::Cache
            && let Some(existing) = self.lock().records.get(&track.video_id)
            && existing.kind == DownloadKind::Download
        {
            return Ok(existing.clone());
        }
        let path = self
            .dir
            .join(kind.dir_name())
            .join(file_name(&track.video_id, mime_type));
        write_atomic(&path, bytes)?;
        let now = now_ms();
        let mut state = self.lock();
        state.access_seq += 1;
        let record = DownloadRecord {
            track: track.clone(),
            kind,
            path,
            size: bytes.len() as u64,
            complete,
            mime_type: mime_type.to_owned(),
            loudness_db,
            created_at: now,
            last_access_at: now,
            access_seq: state.access_seq,
        };
        if let Some(old) = state.records.insert(track.video_id.clone(), record.clone())
            && old.path != record.path
        {
            let _ = std::fs::remove_file(&old.path);
        }
        if kind == DownloadKind::Cache {
            self.evict(&mut state);
        }
        self.save_index(&state);
        drop(state);
        if kind == DownloadKind::Download {
            self.mark_library(track, Some(complete));
        }
        Ok(record)
    }

    fn worker(self: Arc<Self>) {
        loop {
            let (job_id, track, kind) = {
                let mut state = self.lock();
                loop {
                    if state.shutdown {
                        return;
                    }
                    if let Some(id) = state.queue.pop_front() {
                        let job = state.jobs.iter_mut().find(|j| j.id == id);
                        if let Some(job) = job.filter(|j| !j.cancelled) {
                            job.state.status = DownloadStatus::Downloading;
                            break (id, job.state.track.clone(), job.state.kind);
                        }
                        continue;
                    }
                    state = self.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            };

            let fetched = self.resolver.resolve(&track.video_id).and_then(|stream| {
                if let Some(job) = self.lock().jobs.iter_mut().find(|j| j.id == job_id) {
                    job.state.total_bytes = stream.content_length;
                }
                let download = stream.download()?;
                Ok((stream, download))
            });

            let cancelled = self
                .lock()
                .jobs
                .iter()
                .any(|j| j.id == job_id && j.cancelled);
            let outcome = if cancelled {
                Err(None)
            } else {
                fetched
                    .and_then(|(stream, d)| {
                        self.store(
                            &track,
                            kind,
                            &d.bytes,
                            d.complete,
                            &stream.mime_type,
                            stream.loudness_db,
                        )
                    })
                    .map_err(Some)
            };

            let mut state = self.lock();
            if let Some(job) = state.jobs.iter_mut().find(|j| j.id == job_id) {
                match outcome {
                    Ok(record) => {
                        job.state.status = DownloadStatus::Completed;
                        job.state.size = record.size;
                        job.state.complete = record.complete;
                    }
                    Err(None) => job.state.status = DownloadStatus::Cancelled,
                    Err(Some(e)) => {
                        log::warn!("downloads: {} failed: {e}", track.video_id);
                        job.state.status = DownloadStatus::Failed(e.to_string());
                    }
                }
            }
            prune_finished(&mut state);
        }
    }
}

fn prune_finished(state: &mut State) {
    let finished = state
        .jobs
        .iter()
        .filter(|j| j.state.status.is_finished())
        .count();
    let mut excess = finished.saturating_sub(MAX_FINISHED_JOBS);
    state.jobs.retain(|j| {
        if excess > 0 && j.state.status.is_finished() {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

impl DownloadManager {
    /// Opens the store in `dir` (created if needed) and starts the workers. Records whose
    /// file disappeared are dropped. `library`, if given, mirrors user downloads.
    pub fn new(
        dir: impl Into<PathBuf>,
        resolver: Arc<dyn StreamResolver>,
        library: Option<Library>,
    ) -> Result<Self> {
        let dir = dir.into();
        for kind in [DownloadKind::Download, DownloadKind::Cache] {
            std::fs::create_dir_all(dir.join(kind.dir_name()))?;
        }
        let mut records: HashMap<String, DownloadRecord> = match std::fs::read(dir.join(INDEX_FILE))
        {
            Ok(bytes) => serde_json::from_slice::<Vec<DownloadRecord>>(&bytes)
                .unwrap_or_else(|e| {
                    log::warn!("downloads: unreadable index, starting empty: {e}");
                    Vec::new()
                })
                .into_iter()
                .map(|r| (r.track.video_id.clone(), r))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.into()),
        };
        records.retain(|_, r| r.path.is_file());
        let access_seq = records.values().map(|r| r.access_seq).max().unwrap_or(0);

        let inner = Arc::new(Inner {
            dir,
            resolver,
            library,
            state: Mutex::new(State {
                records,
                jobs: Vec::new(),
                queue: VecDeque::new(),
                next_job: 0,
                access_seq,
                cache_limit: DEFAULT_CACHE_LIMIT,
                shutdown: false,
            }),
            wake: Condvar::new(),
        });
        {
            let state = inner.lock();
            inner.save_index(&state);
        }
        for i in 0..CONCURRENCY {
            let inner = inner.clone();
            thread::Builder::new()
                .name(format!("music-core-download-{i}"))
                .spawn(move || inner.worker())?;
        }
        Ok(Self { inner })
    }

    /// Directory holding `downloads/`, `cache/` and `index.json`.
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    fn enqueue(&self, track: Track, kind: DownloadKind) {
        let mut state = self.inner.lock();
        let busy = state.jobs.iter().any(|j| {
            j.state.track.video_id == track.video_id
                && !j.state.status.is_finished()
                && !j.cancelled
                && (j.state.kind == kind || j.state.kind == DownloadKind::Download)
        });
        if busy {
            return;
        }
        state.next_job += 1;
        let id = state.next_job;
        state.jobs.push(Job {
            id,
            state: DownloadState {
                track,
                kind,
                status: DownloadStatus::Queued,
                total_bytes: None,
                size: 0,
                complete: false,
            },
            cancelled: false,
        });
        state.queue.push_back(id);
        drop(state);
        self.inner.wake.notify_one();
    }

    /// Queues a user download. No-op if it is already downloaded or queued; a cached copy is
    /// promoted to a download without refetching.
    pub fn download(&self, track: Track) {
        let mut state = self.inner.lock();
        match state.records.get(&track.video_id).map(|r| r.kind) {
            Some(DownloadKind::Download) => {}
            Some(DownloadKind::Cache) => {
                let mut record = state.records.remove(&track.video_id).unwrap();
                let path = self
                    .inner
                    .dir
                    .join(DownloadKind::Download.dir_name())
                    .join(file_name(&track.video_id, &record.mime_type));
                match std::fs::rename(&record.path, &path) {
                    Ok(()) => {
                        record.path = path;
                        record.kind = DownloadKind::Download;
                        record.track = track.clone();
                        let complete = record.complete;
                        state.records.insert(track.video_id.clone(), record);
                        self.inner.save_index(&state);
                        drop(state);
                        self.inner.mark_library(&track, Some(complete));
                    }
                    Err(e) => {
                        log::warn!("downloads: promoting cached file failed: {e}");
                        drop(state);
                        self.enqueue(track, DownloadKind::Download);
                    }
                }
            }
            None => {
                drop(state);
                self.enqueue(track, DownloadKind::Download);
            }
        }
    }

    /// Queues fetching `track` into the play cache unless a copy already exists.
    pub fn cache(&self, track: Track) {
        if self.inner.lock().records.contains_key(&track.video_id) {
            return;
        }
        self.enqueue(track, DownloadKind::Cache);
    }

    /// Stores audio the player already fetched in the play cache (evicting as needed).
    /// Does nothing if the track is downloaded. Returns the cached file.
    pub fn cache_bytes(
        &self,
        track: &Track,
        bytes: &[u8],
        complete: bool,
        mime_type: &str,
    ) -> Result<Option<PathBuf>> {
        if let Some(r) = self.inner.lock().records.get(&track.video_id)
            && r.kind == DownloadKind::Download
        {
            return Ok(None);
        }
        let record =
            self.inner
                .store(track, DownloadKind::Cache, bytes, complete, mime_type, None)?;
        Ok(record.path.is_file().then_some(record.path))
    }

    /// Cancels queued or running jobs for `video_id` (stored files are kept).
    pub fn cancel(&self, video_id: &str) {
        let mut state = self.inner.lock();
        for job in state
            .jobs
            .iter_mut()
            .filter(|j| j.state.track.video_id == video_id && !j.state.status.is_finished())
        {
            job.cancelled = true;
            if job.state.status == DownloadStatus::Queued {
                job.state.status = DownloadStatus::Cancelled;
            }
        }
    }

    /// Cancels jobs and deletes the stored file (download or cache) for `video_id`.
    pub fn delete(&self, video_id: &str) -> Result<()> {
        self.cancel(video_id);
        let mut state = self.inner.lock();
        if let Some(record) = state.records.remove(video_id) {
            match std::fs::remove_file(&record.path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    state.records.insert(video_id.to_owned(), record);
                    return Err(e.into());
                }
            }
            self.inner.save_index(&state);
            drop(state);
            if record.kind == DownloadKind::Download {
                self.inner.mark_library(&record.track, None);
            }
        }
        Ok(())
    }

    /// Jobs of this session in the order they were queued (finished ones until cleared).
    pub fn states(&self) -> Vec<DownloadState> {
        self.inner
            .lock()
            .jobs
            .iter()
            .map(|j| j.state.clone())
            .collect()
    }

    /// Forgets finished jobs in [`Self::states`].
    pub fn clear_finished(&self) {
        self.inner
            .lock()
            .jobs
            .retain(|j| !j.state.status.is_finished());
    }

    /// Stored files of `kind` (all if `None`), most recently stored first.
    pub fn list(&self, kind: Option<DownloadKind>) -> Vec<DownloadRecord> {
        let mut out: Vec<DownloadRecord> = self
            .inner
            .lock()
            .records
            .values()
            .filter(|r| kind.is_none_or(|k| r.kind == k))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(b.access_seq.cmp(&a.access_seq))
        });
        out
    }

    pub fn record(&self, video_id: &str) -> Option<DownloadRecord> {
        self.inner.lock().records.get(video_id).cloned()
    }

    /// Bytes stored for `kind` (all if `None`).
    pub fn total_size(&self, kind: Option<DownloadKind>) -> u64 {
        self.inner
            .lock()
            .records
            .values()
            .filter(|r| kind.is_none_or(|k| r.kind == k))
            .map(|r| r.size)
            .sum()
    }

    /// The stored file for `video_id` (download first, else cache), marking it recently used.
    pub fn local_file(&self, video_id: &str) -> Option<PathBuf> {
        let mut state = self.inner.lock();
        state.access_seq += 1;
        let seq = state.access_seq;
        let record = state.records.get_mut(video_id)?;
        if !record.path.is_file() {
            return None;
        }
        record.access_seq = seq;
        record.last_access_at = now_ms();
        Some(record.path.clone())
    }

    pub fn cache_limit(&self) -> u64 {
        self.inner.lock().cache_limit
    }

    /// Sets the play cache size limit, evicting least recently used files to fit.
    pub fn set_cache_limit(&self, bytes: u64) {
        let mut state = self.inner.lock();
        state.cache_limit = bytes;
        if !self.inner.evict(&mut state).is_empty() {
            self.inner.save_index(&state);
        }
    }

    /// Deletes every cached (not downloaded) file.
    pub fn clear_cache(&self) {
        let mut state = self.inner.lock();
        state.records.retain(|_, r| {
            let keep = r.kind == DownloadKind::Download;
            if !keep {
                let _ = std::fs::remove_file(&r.path);
            }
            keep
        });
        self.inner.save_index(&state);
    }

    /// Writes the index now (also done on every change and on drop; access times otherwise
    /// only reach disk with the next change).
    pub fn flush(&self) {
        let state = self.inner.lock();
        self.inner.save_index(&state);
    }
}

impl Drop for DownloadManager {
    fn drop(&mut self) {
        let mut state = self.inner.lock();
        state.shutdown = true;
        self.inner.save_index(&state);
        drop(state);
        self.inner.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::ResolvedStream;
    use crate::library::SongScope;
    use crate::library::test_util::{tempdir, track};

    const MIB: usize = 1024 * 1024;

    /// Minimal HTTP server for ranged GETs:
    /// `/full/<len>` serves `len` bytes, `/preview/<len>` 403s every range past the first
    /// MiB, `/slow/<len>` waits 400 ms first, `/missing/…` answers 404.
    struct Server {
        port: u16,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
    }

    impl Server {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let in_flight = Arc::new(AtomicUsize::new(0));
            let max_in_flight = Arc::new(AtomicUsize::new(0));
            let (cur, max) = (in_flight.clone(), max_in_flight.clone());
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (cur, max) = (cur.clone(), max.clone());
                    thread::spawn(move || serve(stream, &cur, &max));
                }
            });
            Server {
                port,
                in_flight,
                max_in_flight,
            }
        }
    }

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn serve(stream: TcpStream, cur: &AtomicUsize, max: &AtomicUsize) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        let mut range = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                let (a, b) = v.trim().split_once('-').unwrap();
                range = Some((a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()));
            }
        }
        let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
        let mut parts = path.trim_start_matches('/').split('/');
        let (mode, len) = (parts.next().unwrap(), parts.next().unwrap_or("0"));
        let len: usize = len.parse().unwrap_or(0);
        let now = cur.fetch_add(1, Ordering::SeqCst) + 1;
        max.fetch_max(now, Ordering::SeqCst);
        if mode == "slow" {
            thread::sleep(Duration::from_millis(400));
        }
        let mut out = stream;
        let (start, end) = range.unwrap_or((0, len.saturating_sub(1)));
        let response = if mode == "missing" {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        } else if mode == "preview" && start >= MIB {
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        } else {
            let data = &body(len)[start..=end.min(len - 1)];
            let mut r = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
            )
            .into_bytes();
            r.extend_from_slice(data);
            r
        };
        let _ = out.write_all(&response);
        cur.fetch_sub(1, Ordering::SeqCst);
    }

    /// Resolves video id `<mode>-<len>` to `/mode/len` on the test server.
    struct TestResolver {
        port: u16,
    }

    impl StreamResolver for TestResolver {
        fn resolve(&self, video_id: &str) -> Result<ResolvedStream> {
            let (mode, len) = video_id.split_once('-').unwrap();
            if mode == "unplayable" {
                return Err(Error::Unplayable("test".into()));
            }
            Ok(ResolvedStream {
                url: format!("http://127.0.0.1:{}/{mode}/{len}", self.port),
                itag: 140,
                mime_type: "audio/mp4; codecs=\"mp4a.40.2\"".into(),
                content_length: len.parse().ok(),
                user_agent: "music-core-test",
                loudness_db: Some(-3.0),
            })
        }
    }

    fn manager(dir: &Path, library: Option<Library>) -> (DownloadManager, Server) {
        let server = Server::start();
        let resolver = Arc::new(TestResolver { port: server.port });
        (
            DownloadManager::new(dir, resolver, library).unwrap(),
            server,
        )
    }

    fn wait_finished(m: &DownloadManager) -> Vec<DownloadState> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let states = m.states();
            if states.iter().all(|s| s.status.is_finished()) {
                return states;
            }
            assert!(Instant::now() < deadline, "timed out: {states:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn t(id: &str) -> Track {
        track(id, id, "Artist", None)
    }

    #[test]
    fn downloads_complete_and_preview_files_and_mirrors_library() {
        let dir = tempdir();
        let lib = Library::open_in_memory().unwrap();
        let (m, _server) = manager(&dir, Some(lib.clone()));
        m.download(t("full-5000"));
        m.download(t("preview-1500000"));
        m.download(t("unplayable-1"));
        m.download(t("missing-10"));
        m.download(t("full-5000")); // duplicate, ignored
        let states = wait_finished(&m);
        assert_eq!(states.len(), 4);
        assert_eq!(states[0].status, DownloadStatus::Completed);
        assert!(states[0].complete);
        assert_eq!(states[0].size, 5000);
        assert_eq!(states[0].total_bytes, Some(5000));
        assert_eq!(states[1].status, DownloadStatus::Completed);
        assert!(!states[1].complete);
        assert_eq!(states[1].size, MIB as u64);
        assert!(matches!(states[2].status, DownloadStatus::Failed(_)));
        assert!(matches!(states[3].status, DownloadStatus::Failed(_)));

        let path = m.local_file("full-5000").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body(5000));
        assert_eq!(path.extension().unwrap(), "m4a");
        let rec = m.record("preview-1500000").unwrap();
        assert!(!rec.complete);
        assert_eq!(rec.loudness_db, Some(-3.0));
        assert_eq!(
            m.total_size(Some(DownloadKind::Download)),
            5000 + MIB as u64
        );
        assert_eq!(m.list(None).len(), 2);

        let song = lib.song("full-5000").unwrap().unwrap();
        assert!(song.downloaded_at.is_some() && song.download_complete);
        assert!(
            !lib.song("preview-1500000")
                .unwrap()
                .unwrap()
                .download_complete
        );
        assert_eq!(lib.song_count(SongScope::Downloaded).unwrap(), 2);

        m.delete("full-5000").unwrap();
        assert!(!path.exists());
        assert!(m.local_file("full-5000").is_none());
        assert_eq!(lib.song_count(SongScope::Downloaded).unwrap(), 1);

        m.clear_finished();
        assert!(m.states().is_empty());

        // Records survive a restart; vanished files are dropped.
        drop(m);
        let resolver = Arc::new(TestResolver { port: 1 });
        let m = DownloadManager::new(&dir, resolver.clone(), None).unwrap();
        assert!(m.local_file("preview-1500000").is_some());
        std::fs::remove_file(m.record("preview-1500000").unwrap().path).unwrap();
        drop(m);
        let m = DownloadManager::new(&dir, resolver, None).unwrap();
        assert!(m.list(None).is_empty());
        drop(m);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn runs_two_at_a_time_and_cancels() {
        let dir = tempdir();
        let (m, server) = manager(&dir, None);
        for i in 0..4 {
            m.download(t(&format!("slow-{}", 1000 + i)));
        }
        m.download(t("slow-2000"));
        // Cancel one that is still queued and, once running, one that is in flight.
        m.cancel("slow-2000");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !m
            .states()
            .iter()
            .any(|s| s.status == DownloadStatus::Downloading)
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        let running = m
            .states()
            .into_iter()
            .find(|s| s.status == DownloadStatus::Downloading)
            .unwrap();
        m.cancel(&running.track.video_id);
        let states = wait_finished(&m);
        assert_eq!(server.max_in_flight.load(Ordering::SeqCst), CONCURRENCY);
        assert_eq!(server.in_flight.load(Ordering::SeqCst), 0);
        let cancelled: Vec<_> = states
            .iter()
            .filter(|s| s.status == DownloadStatus::Cancelled)
            .map(|s| s.track.video_id.clone())
            .collect();
        assert_eq!(cancelled.len(), 2);
        assert!(cancelled.contains(&"slow-2000".to_owned()));
        assert!(m.local_file(&running.track.video_id).is_none());
        assert_eq!(m.list(Some(DownloadKind::Download)).len(), 3);
        drop(m);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cache_is_lru_evicted_but_downloads_are_not() {
        let dir = tempdir();
        let (m, _server) = manager(&dir, None);
        m.set_cache_limit(250);
        assert_eq!(m.cache_limit(), 250);
        let data = [1u8; 100];
        m.cache_bytes(&t("a"), &data, true, "audio/mp4").unwrap();
        m.cache_bytes(&t("b"), &data, true, "audio/mp4").unwrap();
        // Touch "a" so "b" is the least recently used.
        assert!(m.local_file("a").is_some());
        m.cache_bytes(&t("c"), &data, false, "audio/mp4").unwrap();
        assert!(m.record("b").is_none());
        assert!(m.local_file("a").is_some() && m.local_file("c").is_some());
        assert_eq!(m.total_size(Some(DownloadKind::Cache)), 200);

        // Promote "a" to a download: it moves out of the cache and is never evicted.
        m.download(t("a"));
        let a = m.record("a").unwrap();
        assert_eq!(a.kind, DownloadKind::Download);
        assert!(a.path.starts_with(dir.join("downloads")));
        assert_eq!(std::fs::read(&a.path).unwrap(), data);
        // Downloaded tracks aren't cached again.
        assert_eq!(
            m.cache_bytes(&t("a"), &data, true, "audio/mp4").unwrap(),
            None
        );

        m.set_cache_limit(0);
        assert!(m.record("c").is_none());
        assert!(m.record("a").is_some());
        let files = std::fs::read_dir(dir.join("cache")).unwrap().count();
        assert_eq!(files, 0);

        m.set_cache_limit(10_000);
        m.cache_bytes(&t("d"), &data, true, "audio/webm").unwrap();
        assert_eq!(m.local_file("d").unwrap().extension().unwrap(), "webm");
        m.clear_cache();
        assert_eq!(m.total_size(Some(DownloadKind::Cache)), 0);
        assert_eq!(m.total_size(None), 100);

        // Fetching into the cache through the resolver.
        m.cache(t("full-300"));
        wait_finished(&m);
        assert_eq!(m.record("full-300").unwrap().kind, DownloadKind::Cache);
        drop(m);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_names_are_safe_and_distinct() {
        assert_eq!(file_name("abc_-1", "audio/mp4"), "abc_-1.m4a");
        let a = file_name("local:x/y", "audio/flac");
        let b = file_name("local_x_y", "audio/flac");
        assert!(!a.contains('/') && !a.contains(':'));
        assert_ne!(a, b);
        assert!(file_name("x", "application/octet-stream").ends_with(".bin"));
    }
}
