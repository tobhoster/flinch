//! A loopback HTTP server answering from a closure, one request per
//! connection, recording every request it saw.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }
}

pub struct Server {
    pub base: String,
    pub seen: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    pub fn requests(&self) -> Vec<Request> {
        self.seen.lock().expect("the request log").clone()
    }
}

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((key, value)) = header.split_once(':') {
            headers.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    let length = headers.iter().find(|(key, _)| key.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request { method, path, headers, body: String::from_utf8_lossy(&body).into_owned() })
}

/// Serve until the test process ends.
pub fn serve(mut handler: impl FnMut(&Request) -> (u16, String) + Send + 'static) -> Server {
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
