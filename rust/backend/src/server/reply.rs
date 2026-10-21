//! Writing a reply: the write deadline, the one-writer frame writers and the off-loop job pool.

use super::*;
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;

/// Output of an op handler. The first frame is the response to the request;
/// any additional frames are emitted in order and represent things like ring
/// replay on hello.
pub type HandlerOutput = Vec<(Frame, Option<Vec<u8>>)>;

// Half-open connection reaper tunables (ADR 0027). A peer that dies without a
// FIN — a frontend killed -9, a collapsed SSH local-forward, a yanked network
// — leaves the daemon-side socket ESTAB forever; without these two mechanisms
// its task leaks (fd + ClientGuard) and, if blocked mid-write, never reads the
// socket again (the broadcast-stall we hit). See `write_frame_to`
// (write-timeout). The keepalive half lived in the TCP listener and retired
// with it in 0.4.0 — on the local socket, a dead SSH forward closes the
// stream (EOF) rather than leaving it silently half-open.

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
pub(crate) async fn write_frame_to<W>(tx: &mut W, frame: &Frame, blob: Option<&[u8]>) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    write_frame_within(tx, frame, blob, write_deadline(blob)).await
}

/// Timeout-parameterized core of [`write_frame_to`], split out so the reaper's
/// drop-on-stuck-peer behavior is unit-testable in milliseconds rather than the
/// production `WRITE_TIMEOUT`.
pub(crate) async fn write_frame_within<W>(
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

/// Write one outgoing frame — an inline reply or an off-loop job's reply
/// alike — with the SAME containment `handle_connection`'s dispatch loop has
/// always applied: an over-cap envelope degrades to an error frame for that
/// request instead of ending the connection (`codec::write_frame` validates
/// size before writing a single byte, so nothing reached the wire and the
/// stream is still consistent); every other write failure, most notably the
/// write-timeout "peer not draining" bail, still propagates so the caller's
/// `?` ends the connection exactly as it always did.
pub(super) async fn write_reply<W>(tx: &mut W, frame: Frame, blob: Option<Vec<u8>>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if let Err(e) = write_frame_to(tx, &frame, blob.as_deref()).await {
        if let Some(too_large) = e.downcast_ref::<codec::EnvelopeTooLarge>() {
            tracing::warn!(
                op = %frame.op,
                id = frame.id,
                len = too_large.len,
                cap = too_large.cap,
                "response envelope over cap — answering with error frame, keeping connection"
            );
            let payload = serde_json::json!({
                "error": format!("{too_large}"),
                "code": "envelope_too_large",
            });
            let err_frame = Frame::res(frame.id, &frame.op, payload);
            write_frame_to(tx, &err_frame, None).await?;
            return Ok(());
        }
        return Err(e);
    }
    Ok(())
}

/// Per-request service-time logging (`SLOW_REQUEST_MS`) plus the error
/// containment that turns a handler `Err` into one `handler_error` frame for
/// `req_id` — shared by the inline dispatch path and every off-loop job
/// (`spawn_job`) so the two don't carry separate copies of the same
/// bookkeeping.
pub(super) fn finish_dispatch(
    op_name: &str,
    req_id: u64,
    started: std::time::Instant,
    result: Result<HandlerOutput>,
) -> HandlerOutput {
    let service_ms = started.elapsed().as_millis() as u64;
    if service_ms >= SLOW_REQUEST_MS {
        tracing::info!(op = %op_name, id = req_id, service_ms, "slow request");
    } else {
        tracing::debug!(op = %op_name, id = req_id, service_ms, "request served");
    }
    match result {
        Ok(frames) => frames,
        Err(e) => {
            tracing::warn!(
                op = %op_name,
                id = req_id,
                error = format!("{e:#}"),
                "handler error — answering with error frame, keeping connection"
            );
            let payload = serde_json::json!({
                "error": format!("{e:#}"),
                "code": "handler_error",
            });
            vec![(Frame::res(req_id, op_name, payload), None)]
        }
    }
}

/// One outgoing job reply: a frame and its optional trailing blob. Off-loop
/// jobs (`spawn_job`) have no access to `tx` — it stays owned by
/// `handle_connection`'s own loop, the connection's one writer — so a job
/// hands its finished reply back over this channel instead; the loop drains
/// it and calls `write_reply` itself, same as it does for its own inline
/// replies.
pub(super) type OutTx = mpsc::Sender<(Frame, Option<Vec<u8>>)>;

/// Per-connection cap on concurrently RUNNING pooled jobs (the `POOLED` ops
/// of `server/dispatch.rs`).
/// Names the invariant it protects: one connection's burst of these can't
/// starve the tokio runtime's worker threads for every OTHER connection.
/// Acquired INSIDE each spawned job, never before spawning, so request
/// intake itself is never blocked by the cap — only how many jobs run at
/// once, once already queued. An inline op never touches this semaphore,
/// which is what keeps `pty.*` served even while every slot here is busy
/// (see `switch_latency/main.rs`'s
/// `pty_not_starved::pty_screen_is_served_while_a_real_slow_kernel_request_is_pending`
/// test), and neither does a job without a permit.
pub(super) const OFFLOOP_CONCURRENCY: usize = 4;

/// Cap on how long a queued pooled job may wait for its `job_sem` PERMIT
/// before being discarded outright — never running the handler at all.
/// Without this, N jobs queued behind a live-but-hung kernel each wait
/// successive `OFFLOOP_CONCURRENCY`-sized batches with no overall bound: 40
/// requests could occupy ~10 batches in a row before the last one even
/// starts. Matches `KERNEL_REQUEST_TIMEOUT`'s own bound, so a request that
/// would time out anyway during its internal wait doesn't also waste a
/// queue slot first waiting to even begin.
const OFFLOOP_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A connection's off-loop jobs. A job runs to its end even after its
/// connection closes, as an inline op always did: an op once read has its
/// effect, and only its reply is lost. So the set detaches its jobs when it is
/// dropped rather than aborting them, and each ends on its own bounds.
pub(super) struct Jobs(pub(super) JoinSet<()>);

impl Drop for Jobs {
    fn drop(&mut self) {
        self.0.detach_all();
    }
}

impl std::ops::Deref for Jobs {
    type Target = JoinSet<()>;
    fn deref(&self) -> &JoinSet<()> {
        &self.0
    }
}

impl std::ops::DerefMut for Jobs {
    fn deref_mut(&mut self) -> &mut JoinSet<()> {
        &mut self.0
    }
}

/// Spawn one request's handler as its own task, off this connection's
/// read/dispatch loop (switch-latency Phase 1): a slow request no longer
/// delays a later cheap request's reply on the same connection. `fut` is
/// the actual handler call, already bound to its own owned copies of
/// whatever it needs (this task outlives the loop iteration that spawned
/// it). A panic inside `fut` is caught and answered like any handler error
/// (`"<op> panicked: <payload>"`), so the request's waiter always gets its
/// reply. Delivers its result through `out_tx` for the loop to write,
/// exactly like an inline reply.
///
/// A pooled job (`semaphore` given) waits for a permit first, and the
/// deadline (`OFFLOOP_QUEUE_TIMEOUT`) starts HERE, before the permit is even
/// requested — not after acquiring it — so a job queued behind a saturated
/// cap for too long is discarded with the standard timeout error and `fut`
/// never runs, rather than finally starting once nobody still cares about
/// the answer. A job without a permit (`None`) runs at once.
pub(super) fn spawn_job<F>(
    jobs: &mut JoinSet<()>,
    semaphore: Option<Arc<Semaphore>>,
    out_tx: OutTx,
    req_id: u64,
    op_name: String,
    fut: F,
) where
    F: std::future::Future<Output = Result<HandlerOutput>> + Send + 'static,
{
    let started = std::time::Instant::now();
    jobs.spawn(async move {
        let permit = match semaphore {
            None => None,
            Some(semaphore) => match tokio::time::timeout(OFFLOOP_QUEUE_TIMEOUT, semaphore.acquire_owned()).await {
                Ok(p) => Some(p.expect("connection job semaphore is never closed")),
                Err(_) => {
                    let err = anyhow::anyhow!(
                        "{op_name} timed out after {OFFLOOP_QUEUE_TIMEOUT:?} waiting to run \
                         (off-loop queue saturated)"
                    );
                    let out_frames = finish_dispatch(&op_name, req_id, started, Err(err));
                    for (frame, blob) in out_frames {
                        let _ = out_tx.send((frame, blob)).await;
                    }
                    return;
                }
            },
        };
        let served = match AssertUnwindSafe(fut).catch_unwind().await {
            Ok(served) => served,
            Err(payload) => {
                let text = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "a non-text payload".to_string());
                Err(anyhow::anyhow!("{op_name} panicked: {text}"))
            }
        };
        let out_frames = finish_dispatch(&op_name, req_id, started, served);
        drop(permit);
        for (frame, blob) in out_frames {
            if out_tx.send((frame, blob)).await.is_err() {
                // The connection loop is gone — nothing left to deliver.
                break;
            }
        }
    });
}

/// Resolve `payload`'s `workspace_id` hint to a concrete workspace INLINE,
/// before a request is handed to a pooled job, and rewrite the hint to
/// that workspace's canonical id. Without this, a job queued behind others
/// (semaphore contention) resolves its workspace only once it actually runs
/// — if the hinted slug's workspace was destroyed and a new one created
/// reusing the SAME slug in the meantime, the job would silently bind to the
/// replacement. Canonical ids are never reused, so re-resolving by id at
/// execution time (the handler's own first step) either finds the SAME
/// workspace or correctly reports it gone — never someone else's. A payload
/// that names no workspace is left as it is, for its handler.
///
/// Returns `Ok(true)` with `payload` rewritten when resolution succeeds,
/// `Ok(false)` after already answering the same `unknown_workspace` error
/// frame the handler itself would have sent — the caller just `continue`s
/// without spawning anything. `Err` only on a write failure serious enough
/// to end the connection.
pub(super) async fn canonicalize_workspace_id<W>(
    tx: &mut W,
    workspaces: &Workspaces,
    req_id: u64,
    op_name: &str,
    payload: &mut serde_json::Value,
) -> Result<bool>
where
    W: AsyncWrite + Unpin,
{
    let hint = payload
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if hint.is_none() {
        return Ok(true);
    }
    match crate::rows::row_or_reply(workspaces, hint.as_deref(), req_id, op_name) {
        Ok(ws) => {
            if let serde_json::Value::Object(m) = payload {
                m.insert(
                    "workspace_id".to_string(),
                    serde_json::Value::String(ws.workspace_id.clone()),
                );
            }
            Ok(true)
        }
        Err(reply) => {
            for (frame, blob) in reply {
                write_reply(tx, frame, blob).await?;
            }
            Ok(false)
        }
    }
}

/// A request whose handler held the connection loop at least this long is
/// logged at info with its op and service time (see the dispatch timer in
/// `handle_connection`). 50 ms is well above any cheap op and well below a
/// switch the user can feel.
const SLOW_REQUEST_MS: u64 = 50;

#[cfg(test)]
#[path = "reply_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "reply_job_panic_tests.rs"]
mod job_panic_tests;
