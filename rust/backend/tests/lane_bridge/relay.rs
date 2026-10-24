//! The test relay is a faithful stand-in for the link: when one side ends, the other side sees the end.

use super::*;

/// With the throttle on, the relay forwards each direction by hand, so each direction must half-close its destination as
/// it returns, as `copy_bidirectional` does for the unthrottled relay. The row's side here accepts and closes at once;
/// the client, which never writes, reads end of stream within 2 s, not when its own write finally fails.
#[tokio::test]
async fn the_throttled_relay_ends_the_clients_side_when_the_rows_side_ends() {
    let dir = tempfile::Builder::new().prefix("sot-relay-end-").permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700)).tempdir_in("/tmp").expect("stand-in folder");
    let row = dir.path().join("row.sock");
    let listener = UnixListener::bind(&row).expect("bind the stand-in row");
    let accepted = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("the relay dials the stand-in row");
        drop(stream);
    });
    let relay = Relay::start(row).await;
    relay.throttle(1000);

    let mut client = UnixStream::connect(&relay.path).await.expect("dial the relay");
    let mut buf = [0u8; 16];
    let read = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf)).await;
    assert!(matches!(read, Ok(Ok(0))), "the client reads end of stream within 2 s of the row's side ending: {read:?}");
    accepted.await.expect("the stand-in row's accept");
}
