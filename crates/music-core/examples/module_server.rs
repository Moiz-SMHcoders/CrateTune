//! A static file server for trying provider modules locally (e.g. the `echo`
//! test module in `tests/fixtures/modules/echo`).
//!
//! ```sh
//! cargo run -p music-core --example module_server -- <dir> [port]
//! ```
//!
//! Serves `<dir>` over HTTP on 127.0.0.1 (byte ranges supported). For the echo
//! module put a `catalog.json` there (an array of `{id, title, artists, album,
//! durationMs, variants: [{quality, file, mimeType, codec, sampleRate}]}`) and
//! the audio under `<dir>/audio/`, then set the module's "Server URL" to the
//! printed address. Requests are logged (the Authorization header redacted).

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next().map(PathBuf::from) else {
        eprintln!("usage: module_server <dir> [port]");
        std::process::exit(2);
    };
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    println!(
        "serving {} at http://{}/",
        root.display(),
        listener.local_addr().unwrap()
    );
    for stream in listener.incoming().flatten() {
        let root = root.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve(stream, &root) {
                eprintln!("connection: {e}");
            }
        });
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "json" => "application/json",
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "arch" => "application/octet-stream",
        "html" => "text/html",
        _ => "application/octet-stream",
    }
}

fn serve(stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("GET").to_owned();
        let target = parts.next().unwrap_or("/").to_owned();
        let mut range = None;
        let mut auth = false;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
                break;
            }
            let lower = header.to_ascii_lowercase();
            if let Some(r) = lower.strip_prefix("range: bytes=") {
                range = Some(r.trim().to_owned());
            }
            auth |= lower.starts_with("authorization:");
        }
        let path = target.split('?').next().unwrap_or("/");
        let decoded = path.replace("%20", " ");
        let file = root.join(decoded.trim_start_matches('/'));
        let safe = !decoded.split('/').any(|c| c == "..");
        println!(
            "{method} {target}{}{}",
            range
                .as_ref()
                .map(|r| format!(" range={r}"))
                .unwrap_or_default(),
            if auth {
                " (authorization: <redacted>)"
            } else {
                ""
            }
        );
        let opened = (safe && file.is_file())
            .then(|| std::fs::File::open(&file).ok())
            .flatten();
        let Some(mut f) = opened else {
            out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")?;
            continue;
        };
        let len = f.metadata()?.len();
        let (status, start, end) = match range.as_deref().and_then(|r| r.split_once('-')) {
            Some((a, b)) => {
                let start: u64 = a.parse().unwrap_or(0);
                let end: u64 = b
                    .parse()
                    .map(|e: u64| e.min(len - 1))
                    .unwrap_or(len.saturating_sub(1));
                if start >= len {
                    out.write_all(
                        b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\n\r\n",
                    )?;
                    continue;
                }
                ("206 Partial Content", start, end)
            }
            None => ("200 OK", 0, len.saturating_sub(1)),
        };
        let count = if len == 0 { 0 } else { end - start + 1 };
        let mut head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {count}\r\nAccept-Ranges: bytes\r\n",
            content_type(&file)
        );
        if status.starts_with("206") {
            head.push_str(&format!("Content-Range: bytes {start}-{end}/{len}\r\n"));
        }
        head.push_str("\r\n");
        out.write_all(head.as_bytes())?;
        if method != "HEAD" && count > 0 {
            f.seek(SeekFrom::Start(start))?;
            std::io::copy(&mut f.take(count), &mut out)?;
        }
    }
}
