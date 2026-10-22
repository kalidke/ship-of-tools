//! What a daemon-side connection writes: a handler's output, the bounded frame write that drops a peer that cannot
//! drain, and the one error frame for a refused connect. Every service takes these from here, never from the
//! server's connection loop.

use crate::{codec, Frame};
use anyhow::Result;
use tokio::io::AsyncWrite;

/// Output of an op handler. The first frame is the response to the request;
/// any additional frames are emitted in order and represent things like ring
/// replay on hello.
pub type HandlerOutput = Vec<(Frame, Option<Vec<u8>>)>;

/// Base deadline for a single frame write before we treat the peer as dead and
/// drop the connection. Small control frames — even over the SSH tunnel — drain
/// in well under a second, so 10s catches a wedged/non-draining peer fast without
/// parking a connection task. A rare false drop is cheap: the FE reconnects
/// automatically (exponential backoff).
///
/// This is a FLOOR, not the whole story: a legitimate bulk blob (a 71 MB
/// scientific render riding the codec's blob tail) can't drain in 10s over a
/// tunnel, and a flat 10s per-frame cap false-dropped it mid-write — the reaper
/// firing on a transfer that WAS draining, so the preview silently never arrived
/// (2026-06-30, example-paper render_sr.png). The deadline is therefore SCALED by
/// the blob size (see `write_deadline`): base + size/`MIN_BLOB_DRAIN_RATE`.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Floor drain rate we credit a healthy peer for the bulk blob tail. A real
/// transfer sustains far more than this over a tunnel; setting the floor low
/// keeps the size-scaled deadline generous enough never to false-drop a draining
/// blob, while a genuinely wedged peer (zero progress) is still bounded here.
/// 1 MiB/s → a 71 MB render gets ~71s on top of the base floor.
const MIN_BLOB_DRAIN_RATE: u64 = 1024 * 1024; // bytes/sec

/// Per-frame write deadline: the [`WRITE_TIMEOUT`] floor plus one second of grace
/// per `MIN_BLOB_DRAIN_RATE` bytes of blob. Envelope-only / small-blob frames get
/// the tight floor (reaper stays sharp); a large preview blob gets proportional
/// time to drain so a legit transfer isn't reaped mid-write.
fn write_deadline(blob: Option<&[u8]>) -> std::time::Duration {
    let extra = blob.map_or(0, |b| b.len() as u64 / MIN_BLOB_DRAIN_RATE);
    WRITE_TIMEOUT + std::time::Duration::from_secs(extra)
}

/// Write one frame to a connection with a bounded timeout (ADR 0027, reaper
/// half 2). On timeout we return an error so `handle_connection` unwinds and
/// drops the connection: a peer that hasn't drained a single frame in
/// `WRITE_TIMEOUT` is dead or wedged, and a parked write would otherwise hold
/// the task forever — never reading the socket, never releasing its
/// `ClientGuard`. Cancel-safety is irrelevant on the timeout path: we tear the
/// whole socket down, so a partially written frame is moot. Every
/// per-connection evt/response write goes through this.
pub async fn write_frame_to<W>(tx: &mut W, frame: &Frame, blob: Option<&[u8]>) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    write_frame_within(tx, frame, blob, write_deadline(blob)).await
}

/// Timeout-parameterized core of [`write_frame_to`], split out so the reaper's
/// drop-on-stuck-peer behavior is unit-testable in milliseconds rather than the
/// production `WRITE_TIMEOUT`.
pub async fn write_frame_within<W>(
    tx: &mut W,
    frame: &Frame,
    blob: Option<&[u8]>,
    timeout: std::time::Duration,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match tokio::time::timeout(timeout, codec::write_frame(tx, frame, blob)).await {
        Ok(inner) => {
            inner?;
            Ok(())
        }
        Err(_elapsed) => anyhow::bail!(
            "frame write exceeded {timeout:?}; dropping connection (peer not draining)"
        ),
    }
}

/// Write a rejection frame (standard error payload, `op` naming which
/// connect verb refused) and return; the caller closes the connection.
pub async fn reject<W>(tx: &mut W, id: u64, op: &str, code: &str, msg: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::json!({ "error": msg, "code": code });
    let f = Frame::res(id, op, payload);
    codec::write_frame(tx, &f, None).await // flushes internally
}

#[cfg(test)]
#[path = "reply_tests.rs"]
mod tests;
