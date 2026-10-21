use super::*;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn this_accounts_connection_is_served_through_the_real_check() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(sot_log::identity::peer_owner::serve_own(listener, "test", serve_conn));
    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(b"GET /nosuchtoken HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let mut buf = [0u8; 128];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), c.read(&mut buf))
        .await
        .expect("this account's connection must be answered")
        .unwrap();
    let head = String::from_utf8_lossy(&buf[..n]);
    assert!(head.starts_with("HTTP/1.1 404"), "got: {head}");
}
