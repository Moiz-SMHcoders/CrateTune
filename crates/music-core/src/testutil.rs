//! Test helpers: a minimal local HTTP server answering byte-range requests (and,
//! with [`TestServer::start_http`], API fakes that look at headers).

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
    /// Like [`Reply::Bytes`], but the connection drops after `sent` bytes of
    /// the body (a stream cut off mid-download).
    Partial {
        body: Vec<u8>,
        start: u64,
        total: u64,
        sent: usize,
    },
    /// Any status with extra header lines (`"Retry-After: 3"`) and a body.
    Full {
        status: u16,
        headers: Vec<String>,
        body: Vec<u8>,
    },
}

/// One request as [`TestServer::start_http`] sees it.
pub(crate) struct Request {
    pub path: String,
    /// Header lines as sent (`name`, `value`), names as written by the client.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// A header's value (name compared case-insensitively).
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

type Handler = dyn Fn(&Request, u64, Option<u64>) -> Reply + Send + Sync;

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
        Self::serve_with(Arc::new(move |req: &Request, start, end| {
            handler(&req.path, start, end)
        }))
    }

    /// Like [`TestServer::start`], with the request's headers and body (API fakes).
    pub(crate) fn start_http(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        Self::serve_with(Arc::new(move |req: &Request, _, _| handler(req)))
    }

    fn serve_with(handler: Arc<Handler>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
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
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
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
    let request = Request {
        path,
        headers,
        body,
    };
    let reply = match handler(&request, start, end) {
        Reply::Partial {
            body,
            start,
            total,
            sent,
        } => {
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                body.len(),
                start,
                start + body.len() as u64 - 1,
                total
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body[..sent.min(body.len())]);
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
        other => other,
    };
    let response = match reply {
        Reply::Partial { .. } => unreachable!(),
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
