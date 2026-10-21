use super::super::video::{handle_conn, register_video};
use super::*;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::{TcpListener, TcpStream};

#[test]
fn parse_range_forms() {
    let p = |h: &str, t| parse_range(Some(h), t);
    assert_eq!(p("bytes=100-199", 1000), RangeReq::Part { start: 100, end: 199 });
    assert_eq!(p("bytes=100-", 1000), RangeReq::Part { start: 100, end: 999 });
    assert_eq!(p("bytes=-10", 1000), RangeReq::Part { start: 990, end: 999 });
    assert_eq!(p("bytes=900-5000", 1000), RangeReq::Part { start: 900, end: 999 });
    assert_eq!(p("bytes=5-2", 1000), RangeReq::Unsatisfiable);
    assert_eq!(p("bytes=1000-", 1000), RangeReq::Unsatisfiable);
    assert_eq!(p("bytes=0-", 0), RangeReq::Unsatisfiable);
    assert_eq!(parse_range(None, 0), RangeReq::Full);
    assert_eq!(p("bytes=0-1,5-6", 1000), RangeReq::Full);
}

/// The video server used to send `Content-Length: 1` for an empty file
/// (`end - start + 1` with end clamped to 0). Through the real listener.
#[tokio::test]
async fn empty_file_sends_content_length_zero() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("empty.mp4");
    std::fs::write(&f, b"").unwrap();
    let token = register_video(f).expect("register_video");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = handle_conn(stream).await;
        }
    });
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET /{token} HTTP/1.1\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), s.read_to_end(&mut raw))
        .await
        .expect("server did not close the connection")
        .unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    assert!(text.starts_with("HTTP/1.1 200 OK"), "got: {text}");
    assert!(text.contains("Content-Length: 0\r\n"), "got: {text}");
    assert!(text.ends_with("\r\n\r\n"), "body must be empty, got: {text}");
}

/// One reply from `r`: its head, then its body. A HEAD reply has no body, though its Content-Length is the body's.
async fn read_reply(r: &mut BufReader<OwnedReadHalf>, head_only: bool) -> (String, Vec<u8>) {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        assert!(
            r.read_line(&mut line).await.unwrap() > 0,
            "closed inside a head: {head}"
        );
        head.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let length: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .unwrap()
        .parse()
        .unwrap();
    let mut body = vec![0; if head_only { 0 } else { length }];
    r.read_exact(&mut body).await.unwrap();
    (head, body)
}

/// A refused HEAD keeps its connection, so the next reply must start where the HEAD's head ends. Through the real
/// listener, on one kept-alive connection.
#[tokio::test]
async fn a_refused_head_sends_no_body_before_the_next_reply() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("clip.mp4");
    std::fs::write(&f, b"0123456789").unwrap();
    let token = register_video(f).expect("register_video");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = handle_conn(stream).await;
        }
    });
    let (r, mut w) = TcpStream::connect(addr).await.unwrap().into_split();
    let mut r = BufReader::new(r);
    // The token is not granted, so the HEAD is refused: 404 "no such grant", 13 bytes of body.
    w.write_all(b"HEAD /no-such-grant HTTP/1.1\r\n\r\n")
        .await
        .unwrap();
    let (head, body) = read_reply(&mut r, true).await;
    assert!(head.starts_with("HTTP/1.1 404 Not Found"), "got: {head}");
    assert!(head.contains("Content-Length: 13\r\n"), "got: {head}");
    assert!(body.is_empty());
    w.write_all(format!("GET /{token} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let (head, body) = read_reply(&mut r, false).await;
    assert!(head.starts_with("HTTP/1.1 200 OK"), "got: {head}");
    assert_eq!(body, b"0123456789");
}
