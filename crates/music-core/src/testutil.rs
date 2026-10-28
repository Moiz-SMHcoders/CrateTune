//! Test helpers: a minimal local HTTP server answering byte-range requests.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

/// What the handler decides for one request.
pub(crate) enum Reply {
    /// Serve `data[start..end]` as 206 (`total` in Content-Range).
    Bytes {
        body: Vec<u8>,
        start: u64,
        total: u64,
    },
    Status(u16),
    /// Any status with extra header lines (`"Retry-After: 3"`) and a body.
    Full {
        status: u16,
        headers: Vec<String>,
        body: Vec<u8>,
    },
}

type Handler = dyn Fn(&str, u64, Option<u64>) -> Reply + Send + Sync;

/// Serves requests on 127.0.0.1 until dropped (the listener thread is detached).
pub(crate) struct TestServer {
    pub base: String,
    pub requests: Arc<Mutex<Vec<(String, u64)>>>,
}

impl TestServer {
    /// `handler(path_and_query, range_start, range_end_inclusive)`.
    pub(crate) fn start(
        handler: impl Fn(&str, u64, Option<u64>) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let log = requests.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (handler, log) = (handler.clone(), log.clone());
                thread::spawn(move || serve(stream, &*handler, &log));
            }
        });
        Self { base, requests }
    }

    /// Serves `data` with a 206 per range; ranges are clamped to the data.
    pub(crate) fn file(data: Arc<[u8]>) -> Self {
        Self::start(move |_, start, end| range_of(&data, start, end))
    }
}

/// A 206 reply for `start..=end` of `data` (clamped), or 416 past the end.
pub(crate) fn range_of(data: &[u8], start: u64, end: Option<u64>) -> Reply {
    let len = data.len() as u64;
    if start >= len {
        return Reply::Status(416);
    }
    let end = end.unwrap_or(len - 1).min(len - 1);
    Reply::Bytes {
        body: data[start as usize..=end as usize].to_vec(),
        start,
        total: len,
    }
}

fn serve(stream: TcpStream, handler: &Handler, log: &Mutex<Vec<(String, u64)>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let (mut start, mut end) = (0u64, None);
    let mut body_len = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(len) = lower.strip_prefix("content-length:") {
            body_len = len.trim().parse().unwrap_or(0);
        }
        if let Some(range) = lower.strip_prefix("range: bytes=") {
            let (a, b) = range.trim().split_once('-').unwrap_or((range.trim(), ""));
            start = a.parse().unwrap_or(0);
            end = b.parse().ok();
        }
    }
    // Drain a request body (POST) so closing the socket doesn't reset the connection.
    let mut body = vec![0u8; body_len];
    let _ = std::io::Read::read_exact(&mut reader, &mut body);
    log.lock().unwrap().push((path.clone(), start));
    let mut stream = stream;
    let response = match handler(&path, start, end) {
        Reply::Bytes { body, start, total } => {
            let mut head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                body.len(),
                start,
                start + body.len() as u64 - 1,
                total
            )
            .into_bytes();
            head.extend_from_slice(&body);
            head
        }
        Reply::Full {
            status,
            headers,
            body,
        } => {
            let mut head = format!(
                "HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\n",
                body.len()
            );
            for h in headers {
                head.push_str(&h);
                head.push_str("\r\n");
            }
            head.push_str("Connection: close\r\n\r\n");
            let mut head = head.into_bytes();
            head.extend_from_slice(&body);
            head
        }
        Reply::Status(code) => {
            format!("HTTP/1.1 {code} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .into_bytes()
        }
    };
    let _ = stream.write_all(&response);
}
