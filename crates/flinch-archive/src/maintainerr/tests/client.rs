//! The live client over a real socket: a redirect is never followed, because
//! the API key would travel with it.

use super::super::{HttpMaintainerr, MaintainerrApi, MaintainerrError};
use std::io::{Read, Write};
use std::net::TcpListener;

#[tokio::test]
async fn a_redirect_is_an_error_naming_it_and_the_key_never_reaches_its_target() {
    let elsewhere = TcpListener::bind("127.0.0.1:0").expect("bind the redirect target");
    elsewhere.set_nonblocking(true).expect("a target that can be polled");
    let location = format!("http://{}/api/app/status", elsewhere.local_addr().expect("target address"));
    let maintainerr = TcpListener::bind("127.0.0.1:0").expect("bind the fake Maintainerr");
    let base = format!("http://{}", maintainerr.local_addr().expect("fake address"));
    let answer = std::thread::spawn(move || {
        let (mut stream, _) = maintainerr.accept().expect("the client connects");
        // Only the redirect matters; the request just has to arrive first.
        let _request = stream.read(&mut [0u8; 4096]).expect("the request arrives");
        write!(stream, "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("the redirect is sent");
    });

    let result = HttpMaintainerr::new(&base, "secret-key").expect("client").version().await;
    answer.join().expect("the fake Maintainerr finishes");

    assert!(matches!(result, Err(MaintainerrError::Http { status: 302, .. })), "{result:?}");
    assert!(elsewhere.accept().is_err(), "nothing may connect to the redirect target");
}
