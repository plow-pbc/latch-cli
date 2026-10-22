//! The test shop (`tests/browser-shop`) served on 127.0.0.1, for the
//! browser unit tests and the CLI tests alike (each includes this file).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;

/// Serves the shop for as long as the test runs; returns its origin.
pub fn serve_shop() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/browser-shop");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { continue };
            let mut line = String::new();
            BufReader::new(&socket).read_line(&mut line).unwrap_or(0);
            let path = line.split(' ').nth(1).unwrap_or("/");
            let path = path
                .split('?')
                .next()
                .unwrap_or("/")
                .trim_start_matches('/');
            let file = root.join(path);
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
            let _ = socket.write_all(head.as_bytes());
            let _ = socket.write_all(body);
        }
    });
    format!("http://127.0.0.1:{port}")
}
