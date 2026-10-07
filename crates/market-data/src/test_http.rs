//! A tiny routed HTTP/1.1 stub for tests that must exercise real `reqwest`
//! calls without touching Alpaca. Same raw-`TcpListener` approach as
//! `rest.rs`'s pagination test, generalised to route by path and record
//! every request line.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use crate::config::AlpacaConfig;

/// A decoded request target: path plus query pairs (percent-decoded).
#[derive(Debug, Clone)]
pub struct Request {
    pub path: String,
    pub query: Vec<(String, String)>,
}

impl Request {
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn parse(target: &str) -> Request {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect();
    Request {
        path: path.to_string(),
        query,
    }
}

/// Serves `route` on a background thread until the process exits. Returns the
/// base URL and the log of every request received.
pub fn serve(
    route: impl Fn(&Request) -> (u16, String) + Send + 'static,
) -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&log);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let Some(target) = head
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
            else {
                continue;
            };
            let request = parse(target);
            let (status, body) = route(&request);
            seen.lock().unwrap().push(request);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, log)
}

/// An `AlpacaConfig` pointing both REST bases at `base`, with dummy keys.
pub fn config(base: &str) -> AlpacaConfig {
    AlpacaConfig {
        api_key: "test".into(),
        api_secret: "test".into(),
        feed: "sip".into(),
        market_ws: String::new(),
        data_base: base.to_string(),
        trading_base: base.to_string(),
        fmp_api_key: None,
    }
}
