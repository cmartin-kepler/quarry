//! A tiny dependency-free HTTP server for `quarry serve` — enough to list PDFs,
//! trigger a parse on click, and return the rendered view. Single-threaded,
//! GET-only, localhost; this is a local dev tool, not a production server.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

pub struct Req {
    pub path: String,
    pub query: HashMap<String, String>,
}

/// Read one request: parse the request line (`GET /path?query HTTP/1.1`) and drain
/// headers. GET-only, so we ignore the body.
pub fn read_request(stream: &TcpStream) -> std::io::Result<Req> {
    let mut r = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    loop {
        let mut h = String::new();
        let n = r.read_line(&mut h)?;
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
        }
    }
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    let (path, qs) = target.split_once('?').unwrap_or((target, ""));
    let mut query = HashMap::new();
    for pair in qs.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(k), percent_decode(v));
    }
    Ok(Req { path: path.to_string(), query })
}

pub fn write_html(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 3 <= b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode a query value (paths have `/`, spaces, …).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
