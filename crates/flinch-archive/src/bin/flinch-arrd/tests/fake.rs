//! A loopback *arr answering from a closure, one request per connection,
//! recording every request it saw.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub(super) struct Request {
    pub(super) method: String,
    /// With its query.
    pub(super) path: String,
    pub(super) api_key: Option<String>,
}

impl Request {
    /// The path without its query.
    pub(super) fn route(&self) -> &str {
        self.path.split('?').next().unwrap_or_default()
    }
}

pub(super) struct Server {
    pub(super) base: String,
    seen: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    pub(super) fn requests(&self) -> Vec<Request> {
        self.seen.lock().expect("the request log").clone()
    }
}

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?.to_string(), parts.next()?.to_string());
    let (mut api_key, mut length) = (None, 0);
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let Some((name, value)) = header.trim_end().split_once(':') else { break };
        if name.eq_ignore_ascii_case("x-api-key") {
            api_key = Some(value.trim().to_string());
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok()?;
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request { method, path, api_key })
}

/// Serve until the test process ends.
pub(super) fn serve(mut handler: impl FnMut(&Request) -> (u16, String) + Send + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake server");
    let base = format!("http://{}", listener.local_addr().expect("the fake's address"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            // A connection that fails or sends no request has nothing to answer.
            let Ok(mut stream) = stream else { continue };
            let Some(request) = read_request(&stream) else { continue };
            let (status, body) = handler(&request);
            log.lock().expect("the request log").push(request);
            let head = format!(
                "HTTP/1.1 {status} Fake\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            // The client may hang up first; its own assertion reports that.
            stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body.as_bytes())).ok();
        }
    });
    Server { base, seen }
}

/// An instance with an empty library and history: it answers every read.
pub(super) fn empty() -> Server {
    serve(|request| match request.route() {
        "/api/v3/history" => (200, r#"{"totalRecords":0,"records":[]}"#.into()),
        "/api/v3/movie" | "/api/v3/series" | "/api/v3/tag" => (200, "[]".into()),
        _ => (404, "{}".into()),
    })
}
