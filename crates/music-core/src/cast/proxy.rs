//! A small HTTP server in the app that hands media to the Cast receiver.
//!
//! googlevideo URLs only work for a client that looks like the one they were
//! minted for (its User-Agent), often only once a pre-roll wait is over, and in
//! byte ranges; a receiver fetching the URL itself sends its own User-Agent.
//! So the app downloads the stream as the player does ([`ResolvedStream::download_into`]:
//! the right headers, chunked ranges, the ad wait, the iOS prefix) into a
//! [`StreamBuffer`] and serves that — with `Range` support, as media elements
//! seek — at an unguessable URL on the LAN. Local files are served from disk.
//!
//! One thread for accepting plus one per request; nothing is cached on disk.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead as _, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::buffer::StreamBuffer;
use crate::{Error, ResolvedStream, Result};

/// How many media the proxy keeps (the current one and the next).
const KEEP: usize = 3;
/// A request may wait this long for bytes still downloading (the ad wait included).
const DATA_TIMEOUT: Duration = Duration::from_secs(60);

enum Source {
    Buffer(Arc<StreamBuffer>),
    File(PathBuf),
}

struct Entry {
    token: String,
    content_type: String,
    source: Source,
}

struct Shared {
    entries: Mutex<VecDeque<Entry>>,
    shutdown: AtomicBool,
}

/// The proxy; dropping it stops serving (and cancels downloads).
pub struct MediaProxy {
    addr: SocketAddr,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl MediaProxy {
    /// Listens on `ip` (the interface the receiver reaches this computer on), any port.
    pub fn start(ip: IpAddr) -> Result<Self> {
        let listener = TcpListener::bind((ip, 0))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            entries: Mutex::new(VecDeque::new()),
            shutdown: AtomicBool::new(false),
        });
        let accept_shared = shared.clone();
        let accept = std::thread::Builder::new()
            .name("cast-proxy".into())
            .spawn(move || accept_loop(&listener, &accept_shared))
            .map_err(Error::Io)?;
        Ok(Self {
            addr,
            shared,
            accept: Some(accept),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    fn add(&self, source: Source, content_type: String) -> String {
        let token = crate::together::new_id();
        let mut entries = self
            .shared
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        entries.push_back(Entry {
            token: token.clone(),
            content_type,
            source,
        });
        while entries.len() > KEEP {
            if let Some(Entry {
                source: Source::Buffer(buffer),
                ..
            }) = entries.pop_front()
            {
                buffer.cancel();
            }
        }
        format!("http://{}/media/{token}", self.addr)
    }

    /// Starts downloading `stream` and returns the URL the receiver plays.
    pub fn serve_stream(&self, stream: ResolvedStream) -> String {
        let buffer = StreamBuffer::new(stream.content_length);
        buffer.set_read_timeout(DATA_TIMEOUT);
        let download = buffer.clone();
        let content_type = content_type_of(&stream.mime_type);
        std::thread::spawn(move || stream.download_into(&download));
        self.add(Source::Buffer(buffer), content_type)
    }

    /// Serves a local file.
    pub fn serve_file(&self, path: PathBuf) -> String {
        let content_type = content_type_for_path(&path).to_owned();
        self.add(Source::File(path), content_type)
    }
}

impl Drop for MediaProxy {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        for entry in self
            .shared
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            if let Source::Buffer(buffer) = entry.source {
                buffer.cancel();
            }
        }
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }
}

/// `audio/mp4; codecs="mp4a.40.2"` → `audio/mp4` (what a LOAD wants).
pub fn content_type_of(mime: &str) -> String {
    let base = mime.split(';').next().unwrap_or("").trim();
    if base.is_empty() {
        "audio/mp4".into()
    } else {
        base.to_owned()
    }
}

pub fn content_type_for_path(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("ogg" | "oga" | "opus") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("webm") => "audio/webm",
        Some("aac") => "audio/aac",
        _ => "audio/mp4",
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    while !shared.shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let shared = shared.clone();
                let _ = std::thread::Builder::new()
                    .name("cast-proxy-req".into())
                    .spawn(move || {
                        if let Err(e) = serve(stream, &shared) {
                            log::debug!("cast proxy: {e}");
                        }
                    });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` → inclusive range within `total`.
pub fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let spec = header
        .trim()
        .strip_prefix("bytes=")?
        .split(',')
        .next()?
        .trim();
    let (a, b) = spec.split_once('-')?;
    if total == 0 {
        return None;
    }
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (total.saturating_sub(n), total - 1)
        }
        (a, "") => (a.parse().ok()?, total - 1),
        (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(total - 1)),
    };
    (start <= end && start < total).then_some((start, end))
}

fn serve(stream: TcpStream, shared: &Shared) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut range = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':')
            && k.trim().eq_ignore_ascii_case("range")
        {
            range = Some(v.trim().to_owned());
        }
    }
    let mut out = stream;
    let reply = |out: &mut TcpStream, status: &str| {
        write!(
            out,
            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    };
    if method != "GET" && method != "HEAD" {
        return reply(&mut out, "405 Method Not Allowed");
    }
    let token = path.strip_prefix("/media/").unwrap_or("");
    let found = {
        let entries = shared
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        entries.iter().find(|e| e.token == token).map(|e| {
            (
                e.content_type.clone(),
                match &e.source {
                    Source::Buffer(b) => Source::Buffer(b.clone()),
                    Source::File(p) => Source::File(p.clone()),
                },
            )
        })
    };
    let Some((content_type, source)) = found else {
        return reply(&mut out, "404 Not Found");
    };
    // The total size: announced by the stream (wait a little if not yet), or the file's.
    let (mut body, total): (Box<dyn ReadSeek>, Option<u64>) = match source {
        Source::Buffer(buffer) => {
            let deadline = Instant::now() + Duration::from_secs(20);
            while buffer.total_len().is_none()
                && buffer.end().is_none()
                && Instant::now() < deadline
            {
                buffer.wait_for(buffer.len() + 1, Duration::from_millis(200));
            }
            let total = buffer.total_len();
            (Box::new(buffer.reader()), total)
        }
        Source::File(path) => {
            let file = File::open(&path)?;
            let total = file.metadata()?.len();
            (Box::new(file), Some(total))
        }
    };
    let range = match (range.as_deref(), total) {
        (Some(r), Some(total)) => match parse_range(r, total) {
            Some(r) => Some(r),
            None => {
                return write!(
                    out,
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        },
        _ => None,
    };
    let mut head = String::new();
    let length = match (range, total) {
        (Some((a, b)), Some(total)) => {
            head.push_str("HTTP/1.1 206 Partial Content\r\n");
            head.push_str(&format!("Content-Range: bytes {a}-{b}/{total}\r\n"));
            Some(b - a + 1)
        }
        (_, total) => {
            head.push_str("HTTP/1.1 200 OK\r\n");
            total
        }
    };
    head.push_str(&format!(
        "Content-Type: {content_type}\r\nAccept-Ranges: bytes\r\n\
         Access-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n"
    ));
    if let Some(length) = length {
        head.push_str(&format!("Content-Length: {length}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    if method == "HEAD" {
        return out.flush();
    }
    let start = range.map_or(0, |(a, _)| a);
    body.seek(SeekFrom::Start(start))?;
    let mut left = length;
    let mut piece = vec![0u8; 64 * 1024];
    loop {
        let want = left.map_or(piece.len(), |l| (l as usize).min(piece.len()));
        if want == 0 {
            break;
        }
        let n = match body.read(&mut piece[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        out.write_all(&piece[..n])?;
        if let Some(l) = &mut left {
            *l -= n as u64;
        }
    }
    out.flush()
}

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::testutil::TestServer;

    fn get(url: &str, range: Option<&str>) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut req = crate::http::agent()
            .get(url)
            .config()
            .http_status_as_error(false)
            .build();
        if let Some(r) = range {
            req = req.header("Range", r);
        }
        let mut resp = req.call().unwrap();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
            .collect();
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_vec().unwrap_or_default();
        (status, headers, body)
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=90-500", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
        assert_eq!(content_type_of("audio/webm; codecs=\"opus\""), "audio/webm");
        assert_eq!(
            content_type_for_path(std::path::Path::new("/x/a.FLAC")),
            "audio/flac"
        );
    }

    #[test]
    fn serves_a_stream_with_ranges_and_a_file() {
        // The "googlevideo" upstream: byte ranges only.
        let data: Arc<[u8]> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let upstream = TestServer::file(data.clone());
        let proxy = MediaProxy::start(IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap();
        let url = proxy.serve_stream(ResolvedStream {
            url: format!("{}/videoplayback?itag=140", upstream.base),
            itag: 140,
            mime_type: "audio/mp4; codecs=\"mp4a.40.2\"".into(),
            content_length: Some(data.len() as u64),
            user_agent: "com.google.ios.youtube/20.10.4",
            chunk_size: Some(64 * 1024),
            full_access: true,
            ..ResolvedStream::default()
        });
        assert!(url.starts_with(&format!("http://{}/media/", proxy.addr())));

        let (status, headers, body) = get(&url, None);
        assert_eq!(status, 200);
        assert_eq!(header(&headers, "content-type"), Some("audio/mp4"));
        assert_eq!(header(&headers, "accept-ranges"), Some("bytes"));
        assert_eq!(body.as_slice(), &data[..]);

        let (status, headers, body) = get(&url, Some("bytes=250000-"));
        assert_eq!(status, 206);
        assert_eq!(
            header(&headers, "content-range"),
            Some("bytes 250000-299999/300000")
        );
        assert_eq!(body.as_slice(), &data[250_000..]);
        let (status, _, _) = get(&url, Some("bytes=400000-"));
        assert_eq!(status, 416);
        let (status, _, _) = get(&format!("http://{}/media/nope", proxy.addr()), None);
        assert_eq!(status, 404);

        let path = std::env::temp_dir().join(format!("cast-proxy-{}.flac", std::process::id()));
        std::fs::write(&path, b"fLaC0123456789").unwrap();
        let file_url = proxy.serve_file(path.clone());
        let (status, headers, body) = get(&file_url, Some("bytes=4-7"));
        assert_eq!((status, body.as_slice()), (206, &b"0123"[..]));
        assert_eq!(header(&headers, "content-type"), Some("audio/flac"));
        let _ = std::fs::remove_file(path);
    }
}
