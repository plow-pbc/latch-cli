//! The test shop (`tests/browser-shop`) served on 127.0.0.1, for the
//! browser unit tests and the CLI tests alike (each includes this file).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Serves the shop for as long as the test runs; returns its origin.
#[allow(dead_code)]
pub fn serve_shop() -> String {
    serve_shop_recording().0
}

/// `serve_shop`, also returning every return the shop received: the body
/// of each `POST /returns`, which answers with `return-submitted.html`.
#[allow(dead_code)]
pub fn serve_shop_recording() -> (String, Arc<Mutex<Vec<String>>>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/browser-shop");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let returns = Arc::new(Mutex::new(Vec::new()));
    let received = returns.clone();
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(socket) = socket else { continue };
            let mut reader = BufReader::new(&socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap_or(0);
            let mut words = line.split(' ');
            let method = words.next().unwrap_or("GET").to_string();
            let path = words.next().unwrap_or("/");
            let path = path
                .split('?')
                .next()
                .unwrap_or("/")
                .trim_start_matches('/')
                .to_string();
            // The headers, for a body's length.
            let mut length = 0;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let path = if method == "POST" && path == "returns" {
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                received
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&body).into_owned());
                "return-submitted.html".to_string()
            } else {
                path
            };
            let file = root.join(&path);
            let found = (!path.split('/').any(|part| part == ".."))
                .then(|| std::fs::read(&file).ok())
                .flatten();
            let kind = match file.extension().and_then(|e| e.to_str()) {
                Some("js") => "text/javascript",
                Some("css") => "text/css",
                Some("json") => "application/json",
                _ => "text/html; charset=utf-8",
            };
            let (status, body) = match &found {
                Some(body) => ("200 OK", body.as_slice()),
                None => ("404 Not Found", &b""[..]),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let mut out = &socket;
            let _ = out.write_all(head.as_bytes());
            let _ = out.write_all(body);
        }
    });
    (format!("http://127.0.0.1:{port}"), returns)
}
