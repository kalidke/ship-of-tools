use super::*;
use std::time::Duration;
use tokio::io::duplex;

/// The request heads that decide whether a connection is kept, and what each decides.
#[tokio::test]
async fn a_request_keeps_its_connection_only_when_it_may() {
    let cases = [
        ("GET / HTTP/1.1\r\n\r\n", true),
        ("GET / HTTP/1.1\r\nContent-Length: 0\r\n\r\n", true),
        ("GET / HTTP/1.0\r\n\r\n", false),
        ("GET / HTTP/1.1\r\nConnection: close\r\n\r\n", false),
        ("GET / HTTP/1.1\r\nconnection: Keep-Alive, Close\r\n\r\n", false),
        ("GET / HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello", false),
        ("GET / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n", false),
    ];
    for (head, keep) in cases {
        let (mut client, mut server) = duplex(4096);
        client.write_all(head.as_bytes()).await.unwrap();
        let request = read_request(&mut server, &mut Vec::new()).await.unwrap().expect("a request");
        assert_eq!(request.keep, keep, "{head:?}");
    }
}

/// Two requests sent at once are read one after the other from the same buffer.
#[tokio::test]
async fn pipelined_requests_are_read_in_turn() {
    let (mut client, mut server) = duplex(4096);
    client.write_all(b"GET /a HTTP/1.1\r\n\r\nGET /b HTTP/1.1\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut buf = Vec::new();
    let first = read_request(&mut server, &mut buf).await.unwrap().unwrap();
    let second = read_request(&mut server, &mut buf).await.unwrap().unwrap();
    assert_eq!((first.target.as_str(), first.keep), ("/a", true));
    assert_eq!((second.target.as_str(), second.keep), ("/b", false));
}

fn get(keep: bool) -> Request {
    Request { method: "GET".into(), target: "/".into(), range: None, cookie: None, keep }
}

/// A file of a megabyte served through a 1 KiB pipe: the file is read at most a chunk ahead of the client.
/// The guard removes the file when the test ends, however it ends.
async fn big_file() -> (tempfile::TempPath, tokio::fs::File) {
    let path = tempfile::Builder::new().prefix("sot-keep-test-").tempfile().unwrap().into_temp_path();
    std::fs::write(&path, vec![7u8; 1 << 20]).unwrap();
    let file = tokio::fs::File::open(&path).await.unwrap();
    (path, file)
}

/// Read from `client` up to the end of the reply's head.
async fn read_head(client: &mut tokio::io::DuplexStream) {
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        assert_eq!(client.read(&mut b).await.unwrap(), 1);
        head.push(b[0]);
    }
}

/// A reply kept open is whole; one that could not be sent whole (the file shrank, the client left) ends the
/// connection, since the client would read the next reply as the rest of its body.
#[tokio::test]
async fn a_reply_keeps_its_connection_only_when_sent_whole() {
    let timeout = Duration::from_secs(10);
    // Whole.
    let (_path, file) = big_file().await;
    let (mut client, mut server) = duplex(1024);
    let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
    let mut raw = Vec::new();
    tokio::time::timeout(timeout, async {
        read_head(&mut client).await;
        let mut body = vec![0; 1 << 20];
        client.read_exact(&mut body).await.unwrap();
        raw = body;
    })
    .await
    .unwrap();
    assert!(raw.iter().all(|&b| b == 7));
    assert!(tokio::time::timeout(timeout, sent).await.unwrap().unwrap(), "a whole reply keeps the connection");
    // Short: the file shrinks after its head is sent.
    let (shrinking, file) = big_file().await;
    let (mut client, mut server) = duplex(1024);
    let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
    read_head(&mut client).await;
    std::fs::write(&shrinking, b"").unwrap();
    let mut rest = Vec::new();
    tokio::time::timeout(timeout, client.read_to_end(&mut rest)).await.unwrap().unwrap();
    assert!(!tokio::time::timeout(timeout, sent).await.unwrap().unwrap(), "a short reply closes the connection");
    // Failed: the client leaves after the head.
    let (_failing, file) = big_file().await;
    let (mut client, mut server) = duplex(1024);
    let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
    read_head(&mut client).await;
    drop(client);
    assert!(!tokio::time::timeout(timeout, sent).await.unwrap().unwrap(), "a failed reply closes the connection");
}

/// The bound is one deadline for a whole head: one sent a line every 10 s is closed 30 s after the wait began,
/// not 30 s after its last byte.
#[tokio::test(start_paused = true)]
async fn a_head_dripped_past_the_bound_is_closed_at_the_bound() {
    let (mut client, mut server) = duplex(64 * 1024);
    tokio::spawn(async move {
        let _ = client.write_all(b"GET / HTTP/1.1\r\n").await;
        for i in 0u32.. {
            tokio::time::sleep(Duration::from_secs(10)).await;
            if client.write_all(format!("X-Drip-{i}: y\r\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let start = tokio::time::Instant::now();
    assert!(read_request(&mut server, &mut Vec::new()).await.unwrap().is_none());
    let waited = start.elapsed();
    assert!(waited >= IDLE_BOUND && waited < IDLE_BOUND + Duration::from_secs(5), "closed after {waited:?}");
}

/// A client that stops reading mid-reply is cut after `WRITE_BOUND`, and the connection is not kept; one that
/// keeps taking a chunk every 20 s gets the whole reply.
#[tokio::test(start_paused = true)]
async fn a_client_that_stops_reading_is_cut_at_the_write_bound() {
    let (_path, file) = big_file().await;
    let (mut client, mut server) = duplex(1024);
    let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
    read_head(&mut client).await;
    let start = tokio::time::Instant::now();
    let kept = tokio::time::timeout(WRITE_BOUND + Duration::from_secs(5), sent).await.expect("cut at the bound").unwrap();
    assert!(!kept);
    assert!(start.elapsed() >= WRITE_BOUND, "cut after {:?}", start.elapsed());
    drop(client);
    let (_path2, file) = big_file().await;
    let (mut client, mut server) = duplex(READ_CHUNK);
    let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
    read_head(&mut client).await;
    let mut got = 0;
    let mut buf = vec![0u8; 64 * 1024];
    while got < 1 << 20 {
        tokio::time::sleep(Duration::from_secs(20)).await;
        got += client.read(&mut buf).await.unwrap();
    }
    assert!(sent.await.unwrap(), "a slow client that keeps reading gets the whole reply");
}
