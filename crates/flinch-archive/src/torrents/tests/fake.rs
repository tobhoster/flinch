//! A scripted HTTP server over a real socket: one connection per reply, in
//! order, recording each request's line, headers and body.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) headers: Vec<(&'static str, String)>,
    pub(super) body: String,
}

pub(super) fn reply(status: u16, body: &str) -> Reply {
    Reply { status, headers: Vec::new(), body: body.to_string() }
}

impl Reply {
    pub(super) fn with(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_string()));
        self
    }
}

#[derive(Debug)]
pub(super) struct Seen {
    pub(super) line: String,
    /// The header block, lower-cased.
    pub(super) head: String,
    pub(super) body: String,
}

pub(super) fn serve(script: Vec<Reply>) -> (String, JoinHandle<Vec<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake client");
    let base = format!("http://{}", listener.local_addr().expect("fake address"));
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for reply in script {
            let (mut stream, _) = listener.accept().expect("the client connects");
            let mut request = Vec::new();
            let mut buffer = [0u8; 8192];
            let head_end = loop {
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
                let read = stream.read(&mut buffer).expect("the request arrives");
                assert!(read > 0, "the request ended before its headers");
                request.extend_from_slice(&buffer[..read]);
            };
            let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while request.len() < head_end + length {
                let read = stream.read(&mut buffer).expect("the body arrives");
                assert!(read > 0, "the request ended before its body");
                request.extend_from_slice(&buffer[..read]);
            }
            let text = String::from_utf8_lossy(&request).to_string();
            seen.push(Seen { line: text.lines().next().unwrap_or_default().to_string(), head, body: text[head_end..].to_string() });
            let mut answer = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", reply.status, reply.body.len());
            for (name, value) in &reply.headers {
                answer.push_str(&format!("{name}: {value}\r\n"));
            }
            write!(stream, "{answer}\r\n{}", reply.body).expect("the answer is sent");
        }
        seen
    });
    (base, handle)
}

pub(super) fn client() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("client")
}
