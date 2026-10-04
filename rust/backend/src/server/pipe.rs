//! After a connect frame is answered, a connection is a raw byte pipe copied both ways until either side
//! closes; a refused connect gets one error frame.

use anyhow::Result;
use sot_protocol::{codec, Frame};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// The post-handshake body shared by every daemon-side pipe: once a
/// connect frame has been answered `{ok: true, ...}`, the connection
/// stops being a frame stream and becomes a raw byte pipe between the
/// client (`rx`/`tx`, still split because the reader is a `BufReader`
/// wrapping the original connection while the writer is its own half)
/// and `upstream` (one duplex value — a `TcpStream` for `proxy.connect`,
/// a Unix socket or named-pipe client for `lane.connect`). `what` names
/// the connection in the two teardown `debug!` lines (`proxy.connect`'s
/// own port, `lane.connect`'s target+lane) — the caller's concern, not
/// this function's. Returns once either direction closes; the daemon
/// never decodes a byte after the handshake.
pub(crate) async fn pipe_bidirectional<R, W, U>(mut rx: R, mut tx: W, upstream: U, what: &str) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    // `tokio::io::split` rather than a type-specific `.split()` (the
    // former manual `TcpStream::split()` borrowed instead of owning) —
    // generic over `U`, so this one body serves every upstream type this
    // daemon pipes to.
    let (mut up_rx, mut up_tx) = tokio::io::split(upstream);
    let client_to_up = async {
        let r = tokio::io::copy(&mut rx, &mut up_tx).await;
        let _ = up_tx.shutdown().await; // half-close so upstream sees EOF
        r
    };
    let up_to_client = async {
        let r = tokio::io::copy(&mut up_rx, &mut tx).await;
        let _ = tx.shutdown().await;
        r
    };
    // Tear down as soon as EITHER direction closes. A `join!` would wait for
    // BOTH, so a half-open peer (upstream EOFs after responding while the
    // client keeps its write half open, or vice versa) would block the other
    // copy forever and leak the task + both sockets — unbounded growth under
    // repeated half-open connections (codex). `select!` drops the losing copy;
    // the stream halves then drop at return, closing both sockets so the
    // stalled peer sees a reset.
    tokio::select! {
        r = client_to_up => tracing::debug!(what, ?r, "pipe: client→upstream closed first"),
        r = up_to_client => tracing::debug!(what, ?r, "pipe: upstream→client closed first"),
    }
    Ok(())
}

/// Write a rejection frame (standard error payload, `op` naming which
/// connect verb refused) and return; the caller closes the connection.
pub(crate) async fn reject<W>(tx: &mut W, id: u64, op: &str, code: &str, msg: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::json!({ "error": msg, "code": code });
    let f = Frame::res(id, op, payload);
    codec::write_frame(tx, &f, None).await // flushes internally
}
