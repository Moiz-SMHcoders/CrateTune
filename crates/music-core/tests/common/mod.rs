//! A tiny HTTP/1.1 test server for the module tests (one thread per connection).
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    /// Lowercase names.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn ok(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), content_type.into())],
            body: body.into(),
        }
    }

    pub fn json(value: &serde_json::Value) -> Self {
        Self::ok("application/json", value.to_string())
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// This response, cut off after `sent` bytes of its body.
    pub fn truncated(mut self, sent: usize) -> Self {
        self.headers.push((TRUNCATE.into(), sent.to_string()));
        self
    }

    pub fn redirect(to: &str) -> Self {
        Self {
            status: 302,
            headers: vec![("Location".into(), to.into())],
            body: Vec::new(),
        }
    }
}

type Handler = dyn Fn(&Req) -> Resp + Send + Sync;

/// A response header the server interprets instead of sending: the connection
/// drops after that many body bytes (see [`Resp::truncated`]).
const TRUNCATE: &str = "X-Test-Truncate";

pub struct Server {
    pub base: String,
    pub log: Arc<Mutex<Vec<Req>>>,
}

impl Server {
    pub fn start(handler: impl Fn(&Req) -> Resp + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let requests = log.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (handler, requests) = (handler.clone(), requests.clone());
                thread::spawn(move || serve(stream, &*handler, &requests));
            }
        });
        Self { base, log }
    }

    pub fn requests(&self, prefix: &str) -> Vec<Req> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.starts_with(prefix))
            .cloned()
            .collect()
    }
}

fn serve(stream: TcpStream, handler: &Handler, log: &Mutex<Vec<Req>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("GET").to_owned();
        let path = parts.next().unwrap_or("/").to_owned();
        let mut headers = HashMap::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = header.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
            }
        }
        let len: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let req = Req {
            method,
            path,
            headers,
            body,
        };
        log.lock().unwrap().push(req.clone());
        let resp = handler(&req);
        let mut out = stream.try_clone().unwrap();
        let mut head = format!(
            "HTTP/1.1 {} X\r\nContent-Length: {}\r\n",
            resp.status,
            resp.body.len()
        );
        // `X-Test-Truncate: N`: send only N bytes of the body, then hang up.
        let mut truncate = None;
        for (k, v) in &resp.headers {
            if k == TRUNCATE {
                truncate = v.parse::<usize>().ok();
                continue;
            }
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        if out.write_all(head.as_bytes()).is_err() {
            return;
        }
        if let Some(n) = truncate {
            let _ = out.write_all(&resp.body[..n.min(resp.body.len())]);
            let _ = out.shutdown(std::net::Shutdown::Both);
            return;
        }
        if req.method != "HEAD" && out.write_all(&resp.body).is_err() {
            return;
        }
    }
}

/// Serves a file honouring `Range: bytes=a-b` / `bytes=a-` (206).
pub fn file_response(req: &Req, data: &[u8], content_type: &str) -> Resp {
    let len = data.len();
    if let Some(range) = req
        .headers
        .get("range")
        .and_then(|r| r.strip_prefix("bytes="))
    {
        let (a, b) = range.split_once('-').unwrap_or((range, ""));
        let start: usize = a.parse().unwrap_or(0);
        let end: usize = b.parse().map(|e: usize| e.min(len - 1)).unwrap_or(len - 1);
        if start >= len {
            return Resp::status(416);
        }
        return Resp {
            status: 206,
            headers: vec![
                ("Content-Type".into(), content_type.into()),
                ("Content-Range".into(), format!("bytes {start}-{end}/{len}")),
            ],
            body: data[start..=end].to_vec(),
        };
    }
    Resp::ok(content_type, data.to_vec())
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A fresh temporary directory.
pub fn temp_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "music-core-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A 16-bit stereo WAV of `secs` seconds of a quiet 440 Hz sine.
pub fn wav(secs: u32, rate: u32) -> Vec<u8> {
    let frames = secs * rate;
    let data_len = frames * 4;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let s = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 3000.0) as i16;
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
