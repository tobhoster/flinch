//! Reading an HTTP response body under a size limit. Every body FLINCH reads
//! from another server comes through here, so a broken or hostile server
//! cannot exhaust the daemon's memory with one answer. A body over the limit
//! is a failed read: the caller's usual failure path (evidence incomplete,
//! items held) applies, never a partial body.

/// The largest response body FLINCH reads: 64 MiB.
///
/// The paged reads stay near a megabyte or two: Plex and Tautulli pages of
/// 500 rows, *arr history pages of 1,000 records. The largest real answers
/// are the unpaged library listings, and Radarr's `/api/v3/movie` (the whole
/// library, about 4 KB per movie) is the biggest of them: 15–30 MB for 3,000
/// to 8,000 movies (Radarr issue #7423). 64 MiB is twice that, and an eighth
/// of flinch-arrd's 512 MiB memory limit, so the body and its parsed form fit
/// together with the rest of a cycle.
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Why a body was not read. Neither variant carries the URL: Tautulli's holds
/// its API key.
#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    #[error("response body is larger than the {limit}-byte limit")]
    TooLarge { limit: usize },
    #[error("response body read failed: {0}")]
    Read(#[source] reqwest::Error),
}

/// The whole body, or an error once it exceeds [`MAX_BODY_BYTES`].
pub async fn read(response: reqwest::Response) -> Result<Vec<u8>, BodyError> {
    read_capped(response, MAX_BODY_BYTES).await
}

/// The whole body as text, invalid UTF-8 replaced as `Response::text` does.
pub async fn read_text(response: reqwest::Response) -> Result<String, BodyError> {
    let body = read(response).await?;
    Ok(String::from_utf8(body).unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()))
}

/// A declared length over `limit` is refused before any of the body is read;
/// an undeclared one is read chunk by chunk and refused as soon as the total
/// passes `limit`.
async fn read_capped(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, BodyError> {
    let declared = response.content_length().map(usize::try_from);
    let capacity = match declared {
        Some(Ok(length)) if length <= limit => length,
        Some(_) => return Err(BodyError::TooLarge { limit }),
        None => 0,
    };
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.map_err(|error| BodyError::Read(error.without_url()))? {
        if chunk.len() > limit - body.len() {
            return Err(BodyError::TooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{read_capped, BodyError};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const LIMIT: usize = 100;

    /// Answer one request on a loopback socket with `head` (status line and
    /// headers, without the blank line) and then `body`, verbatim.
    async fn answer(head: &'static str, body: Vec<u8>) -> Result<Vec<u8>, BodyError> {
        let server = TcpListener::bind("127.0.0.1:0").expect("bind the fake server");
        let url = format!("http://{}/", server.local_addr().expect("server address"));
        let serve = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().expect("the client connects");
            let _request = stream.read(&mut [0u8; 4096]).expect("the request arrives");
            // The client may hang up early once it has refused the body.
            stream.write_all(format!("{head}\r\n\r\n").as_bytes()).and_then(|()| stream.write_all(&body)).ok();
        });
        let response = reqwest::get(&url).await.expect("the response head arrives");
        let result = read_capped(response, LIMIT).await;
        serve.join().expect("the fake server finishes");
        result
    }

    fn chunked(chunks: &[usize]) -> Vec<u8> {
        let mut body = Vec::new();
        for &size in chunks {
            body.extend_from_slice(format!("{size:x}\r\n").as_bytes());
            body.extend(std::iter::repeat_n(b'x', size));
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(b"0\r\n\r\n");
        body
    }

    /// `Some(n)`: an `n`-byte body is read; `None`: refused as too large.
    #[rstest::rstest]
    // No body follows the head: only a refusal before reading names the size;
    // reading would fail on the cut-off body instead.
    #[case::declared_over_the_limit("HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nConnection: close", Vec::new(), None)]
    #[case::undeclared_over_the_limit("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked", chunked(&[60, 41]), None)]
    #[case::undeclared_at_the_limit("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked", chunked(&[60, 40]), Some(100))]
    #[case::declared_under_the_limit("HTTP/1.1 200 OK\r\nContent-Length: 5", b"hello".to_vec(), Some(5))]
    #[tokio::test]
    async fn a_body_over_the_limit_is_refused_whether_or_not_it_is_declared(
        #[case] head: &'static str,
        #[case] body: Vec<u8>,
        #[case] read: Option<usize>,
    ) {
        match (answer(head, body).await, read) {
            (Ok(body), Some(length)) => assert_eq!(body.len(), length),
            (Err(BodyError::TooLarge { limit }), None) => assert_eq!(limit, LIMIT),
            (result, expected) => panic!("expected {expected:?} byte(s), got {result:?}"),
        }
    }
}
