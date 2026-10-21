#[tokio::test]
async fn write_frame_within_times_out_on_stuck_peer() {
    // The reaper's core: a peer whose socket buffer is full because it
    // stopped draining (the half-open / Recv-Q stall we hit) must not park
    // the connection task forever — the bounded write trips and errors so
    // `handle_connection` drops the connection.
    use super::write_frame_within;
    use sot_protocol::Frame;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::AsyncWrite;

    struct StuckWriter;
    impl AsyncWrite for StuckWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    let frame = Frame::evt("test.stall", serde_json::json!({"k": "v"}));
    let res = write_frame_within(
        &mut StuckWriter,
        &frame,
        None,
        std::time::Duration::from_millis(50),
    )
    .await;
    assert!(
        res.is_err(),
        "a non-draining peer must trip the write timeout"
    );
    assert!(res.unwrap_err().to_string().contains("not draining"));
}

// Guards the containment path END TO END, not just at the codec. The write
// loop in `handle_connection` keeps the connection alive for an over-cap
// envelope only because it can `downcast_ref::<EnvelopeTooLarge>()` on what
// `write_frame_within` hands back. The codec-level test proves the codec
// produces the type; this one proves the type still SURVIVES the server's
// own wrapper. Without it, a rewrite of that wrapper could silently send
// every oversize response back to dropping the connection while the whole
// suite stayed green.
//
// Note which rewrites are actually dangerous: adding `.context(...)` is
// SAFE — anyhow searches the whole chain, and this test pins that so nobody
// "fixes" a non-problem. What breaks the downcast is reformatting the error
// into a fresh string (`anyhow!("write failed: {e}")`, `bail!`, or a
// `map_err` that stringifies), which discards the concrete type.
#[tokio::test]
async fn oversize_envelope_stays_downcastable_through_write_frame_within() {
    use super::write_frame_within;
    use anyhow::Context as _;
    use sot_protocol::codec::{EnvelopeTooLarge, MAX_ENVELOPE_BYTES};
    use sot_protocol::Frame;

    let mut sink: Vec<u8> = Vec::new();
    let huge = "x".repeat(MAX_ENVELOPE_BYTES + 1);
    let frame = Frame::res(1, "quarto.open", serde_json::json!({ "html": huge }));
    let err = write_frame_within(
        &mut sink,
        &frame,
        None,
        std::time::Duration::from_secs(5),
    )
    .await
    .expect_err("an over-cap envelope must error");

    assert!(
        err.downcast_ref::<EnvelopeTooLarge>().is_some(),
        "handle_connection's containment matches on this type — if the wrapper \
             stops preserving it, oversize responses silently drop connections again"
    );
    assert!(
        sink.is_empty(),
        "nothing may reach the wire, or containment is unsafe"
    );

    // A `.context()` layer must NOT defeat the match (anyhow walks the chain).
    let wrapped = Err::<(), _>(err).context("write envelope").unwrap_err();
    assert!(
        wrapped.downcast_ref::<EnvelopeTooLarge>().is_some(),
        "context-wrapping is safe; only stringifying the error breaks the downcast"
    );
}

// The timeout bail must NOT be mistaken for an over-cap envelope: a peer
// that stopped draining is a genuinely broken socket and has to stay fatal,
// so containment must not swallow it.
#[tokio::test]
async fn write_timeout_is_not_confused_with_an_oversize_envelope() {
    use super::write_frame_within;
    use sot_protocol::codec::EnvelopeTooLarge;
    use sot_protocol::Frame;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::AsyncWrite;

    struct StuckWriter;
    impl AsyncWrite for StuckWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    let frame = Frame::evt("test.stall", serde_json::json!({"k": "v"}));
    let err = write_frame_within(
        &mut StuckWriter,
        &frame,
        None,
        std::time::Duration::from_millis(50),
    )
    .await
    .expect_err("a non-draining peer must trip the write timeout");
    assert!(
        err.downcast_ref::<EnvelopeTooLarge>().is_none(),
        "a stuck peer must stay fatal — containment must not catch it"
    );
}

#[tokio::test]
async fn write_frame_within_succeeds_on_healthy_peer() {
    // The complement: a sink that drains instantly never trips the timeout,
    // so the reaper can't false-drop a healthy connection.
    use super::write_frame_within;
    use sot_protocol::Frame;

    let mut sink: Vec<u8> = Vec::new();
    let frame = Frame::evt("test.ok", serde_json::json!({"k": "v"}));
    let res =
        write_frame_within(&mut sink, &frame, None, std::time::Duration::from_secs(5)).await;
    assert!(res.is_ok(), "a healthy peer must not trip the timeout");
    assert!(!sink.is_empty(), "frame bytes should have been written");
}

#[test]
fn write_deadline_scales_with_blob_size() {
    // Regression for the false-drop of a legit large preview blob: the
    // deadline must be the tight floor for small/no-blob frames (reaper stays
    // sharp) and grow proportionally for a bulk blob so a draining 71 MB
    // render isn't reaped mid-write.
    use super::{write_deadline, MIN_BLOB_DRAIN_RATE, WRITE_TIMEOUT};
    assert_eq!(write_deadline(None), WRITE_TIMEOUT, "no blob → floor");
    let small = vec![0u8; MIN_BLOB_DRAIN_RATE as usize - 1];
    assert_eq!(
        write_deadline(Some(&small)),
        WRITE_TIMEOUT,
        "sub-rate blob → floor (no grace yet)"
    );
    let big = vec![0u8; 5 * MIN_BLOB_DRAIN_RATE as usize];
    assert_eq!(
        write_deadline(Some(&big)),
        WRITE_TIMEOUT + std::time::Duration::from_secs(5),
        "deadline = floor + size / MIN_BLOB_DRAIN_RATE"
    );
}
