use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Bytes cross both ways, and the pipe ends when the client closes: the upstream sees the half-close (EOF) and the
/// function returns without the upstream closing.
#[tokio::test]
async fn bytes_cross_both_ways_and_the_pipe_ends_when_the_client_closes() {
    let (client, client_side) = tokio::io::duplex(1024);
    let (upstream, upstream_side) = tokio::io::duplex(1024);
    let (rx, tx) = tokio::io::split(client_side);
    let pipe = tokio::spawn(async move { pipe_bidirectional(rx, tx, upstream_side, "test").await });
    let (mut client_rd, mut client_wr) = tokio::io::split(client);
    let (mut up_rd, mut up_wr) = tokio::io::split(upstream);
    client_wr.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    up_rd.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping");
    up_wr.write_all(b"pong").await.unwrap();
    client_rd.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"pong");
    client_wr.shutdown().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), pipe).await.expect("the pipe ended").unwrap().unwrap();
}

/// A half-open peer does not hold the pipe: when the upstream closes while the client keeps its write half open, the
/// function returns and the client sees EOF.
#[tokio::test]
async fn the_pipe_ends_when_the_upstream_closes_though_the_client_stays_open() {
    let (client, client_side) = tokio::io::duplex(1024);
    let (upstream, upstream_side) = tokio::io::duplex(1024);
    let (rx, tx) = tokio::io::split(client_side);
    let pipe = tokio::spawn(async move { pipe_bidirectional(rx, tx, upstream_side, "test").await });
    let (mut client_rd, _client_wr) = tokio::io::split(client);
    drop(upstream);
    tokio::time::timeout(std::time::Duration::from_secs(5), pipe).await.expect("the pipe ended").unwrap().unwrap();
    let mut rest = Vec::new();
    client_rd.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty());
}
