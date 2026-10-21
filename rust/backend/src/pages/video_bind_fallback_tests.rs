use super::*;
use tokio::io::AsyncWriteExt;

const BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// Preferred port taken (an ephemeral squatter standing in for another
/// user's daemon on a shared host) → `spawn` falls back to an
/// OS-assigned port, records it for `bound_video_port()`, and actually
/// serves on it (unknown grant → 404, proving it's OUR server).
#[tokio::test]
async fn spawn_falls_back_to_ephemeral_when_preferred_taken() {
    let squatter = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let taken = squatter.local_addr().unwrap().port();
    spawn(taken).await.expect("fallback bind should succeed");
    let bound = bound_video_port().expect("actual port recorded");
    assert_ne!(bound, taken, "must not claim the squatted port");
    let mut s = TcpStream::connect(("127.0.0.1", bound)).await.unwrap();
    s.write_all(b"GET /nosuchtoken HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 128];
    let n = tokio::time::timeout(BOUND, s.read(&mut buf))
        .await
        .expect("the video server's answer within BOUND")
        .unwrap();
    let head = String::from_utf8_lossy(&buf[..n]);
    assert!(head.contains("404"), "unknown grant must 404, got: {head}");
}
