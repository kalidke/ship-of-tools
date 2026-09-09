// kernel.rs — supervisor for the Julia kernel sidecar.
//
// The kernel is the Julia-aware half of the project (per the architecture
// in CLAUDE.md): plugin host, project introspector, owner of dispatch
// tables and AST hashing. Lives as a separate `julia` subprocess so the
// backend can stay Rust-only while still answering Modules-mode queries,
// AST hashes for concept-annotation provenance, and (eventually) anything
// else that requires `JuliaSyntax` / `Base.loaded_modules` etc.
//
// Wire: same NDJSON envelope shape as the main protocol (`{v, id, kind,
// op, payload}\n`). The supervisor multiplexes Rust callers onto a single
// stdin/stdout, routes responses by request id, and relaunches on death.
//
// Invariant: the persistent supervisor owns the child's WHOLE lifetime —
// spawn through hello through serving through exit. Callers never spawn,
// never kill, and never decide when to retry; they only watch the current
// status and wait, bounded by their OWN deadline (`KERNEL_REQUEST_TIMEOUT`).
// A caller that gives up mid-startup (a dropped connection) simply stops
// watching — the supervisor task, spawned independently via `tokio::spawn`,
// is unaffected and keeps running until NO `Kernel` handle wants it
// anymore (`watch::Sender::closed`, below), at which point it stops and
// its child is reaped.
//
// Spawn command:
//   julia --project=<repo>/julia/kernel \
//         -e 'using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout; project_root="<root>")'

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch, OnceCell};

/// Bound on ONE caller's wait for a reply — covering both "the kernel is
/// still starting" and "the kernel is running but this op is slow" alike,
/// since a caller cannot tell those apart from the outside and shouldn't
/// need to: either way, it must not wait longer than this. A cold
/// `using`-time precompile can legitimately take well past this on a first
/// boot; the supervisor keeps the child running regardless (see the module
/// invariant above) so a LATER caller, once startup finishes, succeeds
/// immediately — this bound only ever costs one caller one message.
const KERNEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Respawn backoff floor: the first failed spawn (or the first failure
/// after a prior successful hello) waits this long before the supervisor
/// tries again.
const RESPAWN_BACKOFF_FLOOR: Duration = Duration::from_millis(250);

/// Respawn backoff ceiling: a persistently broken install is retried no
/// more often than this.
const RESPAWN_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Wire-contract protocol version the backend expects the Julia kernel to speak
/// (ADR 0030 §2). Mirrors `ShipToolsKernel.PROTOCOL_VERSION`. The BE and the
/// Julia bundle ship as a unit, so a mismatch is a belt-and-suspenders signal
/// (a stale kernel checkout) rather than a supported configuration — logged
/// loudly at hello but never kills the kernel.
const KERNEL_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone)]
pub struct Kernel {
    inner: Arc<KernelInner>,
}

struct KernelInner {
    kernel_project: PathBuf,
    project_root: PathBuf,
    /// Current status, broadcast to every caller. The supervisor task is
    /// the ONLY writer; `Kernel::request` callers only read/subscribe.
    status: watch::Sender<Status>,
    /// Held ONLY to keep `status`'s receiver count at least 1 for exactly
    /// as long as this `KernelInner` (i.e. every `Kernel` handle sharing
    /// it) is alive — never read. The supervisor task, which does NOT hold
    /// this `KernelInner` (see `ensure_supervisor_started`), awaits
    /// `status.closed()`; that resolves the instant this receiver drops
    /// with no other outstanding — i.e. exactly when nobody outside the
    /// supervisor wants this kernel anymore. Without it, a destroyed
    /// workspace's kernel process would run forever as an orphan: the
    /// supervisor task would otherwise be the only thing keeping anything
    /// alive at all.
    _keepalive: watch::Receiver<Status>,
    /// Spawns the persistent supervisor task exactly once, lazily, on the
    /// first `request()` call — matches the kernel's long-standing
    /// lazy-spawn contract ("only fires up when the first kernel.request
    /// op arrives") without a separate mutex to guard "have we started yet".
    supervisor_started: OnceCell<()>,
}

/// The supervisor's broadcast view of the kernel child. Every transition is
/// made by the supervisor task alone (see `supervisor_loop`); callers only
/// ever read it.
#[derive(Clone)]
enum Status {
    /// No supervisor loop iteration has published anything yet (right after
    /// construction, before the first `request()` call starts it).
    NotStarted,
    /// A spawn attempt is in flight: the child may not exist yet, may be
    /// running but hasn't answered `kernel.hello`, or (this generation)
    /// never will. Callers wait, bounded by their own deadline; they never
    /// treat this as failure.
    Starting,
    /// `kernel.hello` answered; submissions ride this channel.
    Running(mpsc::Sender<Submission>),
    /// The child is definitively not running — it exited (before or after
    /// answering hello) or failed to spawn at all. `reason` is that
    /// generation's failure detail. The supervisor is already asleep,
    /// timing its own next attempt; callers never trigger one.
    Dead { reason: String },
}

/// A `Kernel::request` that failed because the kernel is not usable RIGHT
/// NOW — as opposed to failing for some other reason (bad op, a wire/
/// protocol error). Handlers downcast an `anyhow::Error` to this
/// (`err.downcast_ref::<KernelUnavailable>()`) to show "Julia kernel
/// unavailable: <this>" instead of a generic error, and to skip any retry
/// of their own. `Display` is the bare technical detail with no added
/// framing — composable into whatever prefix a caller wants.
#[derive(Debug, Clone)]
pub enum KernelUnavailable {
    /// Confirmed dead: spawn failed, or the child exited.
    Dead { reason: String },
    /// Still starting: the caller's own deadline elapsed before the kernel
    /// finished booting. NOT a failure of the kernel itself — a later
    /// request may succeed once startup completes.
    Starting { waited: Duration },
}

impl std::fmt::Display for KernelUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelUnavailable::Dead { reason } => write!(f, "{reason}"),
            KernelUnavailable::Starting { waited } => {
                write!(f, "kernel still starting (no response after {waited:?})")
            }
        }
    }
}

impl std::error::Error for KernelUnavailable {}

struct Submission {
    op: String,
    payload: Value,
    reply: oneshot::Sender<Result<Value>>,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    v: u32,
    id: u64,
    kind: &'a str,
    op: &'a str,
    payload: &'a Value,
}

#[derive(Deserialize)]
struct WireResponse {
    id: u64,
    #[serde(default)]
    op: Option<String>,
    payload: Value,
}

impl Kernel {
    pub fn new(kernel_project: PathBuf, project_root: PathBuf) -> Self {
        let (status, keepalive) = watch::channel(Status::NotStarted);
        Self {
            inner: Arc::new(KernelInner {
                kernel_project,
                project_root,
                status,
                _keepalive: keepalive,
                supervisor_started: OnceCell::new(),
            }),
        }
    }

    /// Default kernel project location — `julia/kernel` resolved for both
    /// layouts (dev checkout / release install) via `paths::resource_dir`
    /// (ADR 0030 §4).
    pub fn default_kernel_project() -> PathBuf {
        crate::paths::resource_dir("julia/kernel")
    }

    /// Send a request to the kernel and wait for the matching response,
    /// bounded end-to-end by `KERNEL_REQUEST_TIMEOUT` — whether that time is
    /// spent waiting for startup or waiting for a slow reply from an
    /// already-running kernel. Never spawns, never kills: see the module
    /// invariant.
    pub async fn request(&self, op: &str, payload: Value) -> Result<Value> {
        self.ensure_supervisor_started().await;
        let deadline = Instant::now() + KERNEL_REQUEST_TIMEOUT;
        let mut status_rx = self.inner.status.subscribe();
        loop {
            let status = status_rx.borrow_and_update().clone();
            match status {
                Status::Running(tx) if !tx.is_closed() => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    return submit_and_await(&tx, op, payload, remaining).await;
                }
                Status::Dead { reason } => {
                    return Err(anyhow::Error::new(KernelUnavailable::Dead { reason }));
                }
                Status::NotStarted | Status::Starting | Status::Running(_) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(anyhow::Error::new(KernelUnavailable::Starting {
                            waited: KERNEL_REQUEST_TIMEOUT,
                        }));
                    }
                    match tokio::time::timeout(remaining, status_rx.changed()).await {
                        Ok(Ok(())) => continue,
                        Ok(Err(_)) => {
                            return Err(anyhow!("kernel status channel closed unexpectedly"))
                        }
                        Err(_) => {
                            return Err(anyhow::Error::new(KernelUnavailable::Starting {
                                waited: KERNEL_REQUEST_TIMEOUT,
                            }))
                        }
                    }
                }
            }
        }
    }

    /// Spawn the persistent supervisor loop exactly once, lazily. Idempotent
    /// under concurrent first callers (`OnceCell`). Captures plain clones —
    /// NOT `self.inner` — so the supervisor task's own lifetime never keeps
    /// `KernelInner` (and thus this handle's owning `Workspace`) artificially
    /// alive; see `_keepalive`'s doc for why that matters.
    async fn ensure_supervisor_started(&self) {
        let kernel_project = self.inner.kernel_project.clone();
        let project_root = self.inner.project_root.clone();
        let status = self.inner.status.clone();
        self.inner
            .supervisor_started
            .get_or_init(|| async move {
                tokio::spawn(supervisor_loop(kernel_project, project_root, status));
            })
            .await;
    }
}

/// Submit one request to an already-`Running` kernel and await its reply,
/// bounded by `timeout` (the CALLER's remaining budget, not a fresh one).
async fn submit_and_await(
    tx: &mpsc::Sender<Submission>,
    op: &str,
    payload: Value,
    timeout: Duration,
) -> Result<Value> {
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(Submission {
        op: op.to_string(),
        payload,
        reply: reply_tx,
    })
    .await
    .map_err(|_| anyhow!("kernel supervisor channel closed"))?;
    match tokio::time::timeout(timeout, reply_rx).await {
        Ok(r) => r.map_err(|_| anyhow!("kernel supervisor dropped reply channel"))?,
        Err(_) => Err(anyhow::Error::new(KernelUnavailable::Starting { waited: timeout })),
    }
}

/// The persistent loop: spawn a generation of the child, run it until it
/// answers hello and then dies (or dies before ever answering), record
/// `Dead` with the appropriate backoff, sleep, repeat — until `status`
/// closes (every `Kernel` handle sharing it has been dropped), at which
/// point this returns and the task ends.
async fn supervisor_loop(kernel_project: PathBuf, project_root: PathBuf, status: watch::Sender<Status>) {
    let mut backoff: Option<Duration> = None;
    loop {
        if status.is_closed() {
            return;
        }
        let _ = status.send(Status::Starting);
        let (reached_running, reason) = run_one_generation(&kernel_project, &project_root, &status).await;
        if reached_running {
            // A generation that answered hello resets the ladder — the NEXT
            // failure (whenever it comes) is a fresh first failure, not a
            // continuation of whatever backoff preceded this success.
            backoff = None;
        }
        if status.is_closed() {
            return;
        }
        let next_backoff = match backoff {
            Some(b) => (b * 2).min(RESPAWN_BACKOFF_CAP),
            None => RESPAWN_BACKOFF_FLOOR,
        };
        backoff = Some(next_backoff);
        tracing::warn!(reason = %reason, next_retry_in = ?next_backoff, "kernel unavailable; will retry after backoff");
        let _ = status.send(Status::Dead { reason });
        tokio::time::sleep(next_backoff).await;
    }
}

/// Resolve + spawn one child, fold `kernel.hello` into the same
/// request/response loop every other op uses (no separate raw exchange),
/// publish `Running` the moment it answers, then keep serving until it
/// dies OR `status` closes (the owning `Kernel` was dropped — `kill_on_drop`
/// reaps the child as `child` goes out of scope on return). Returns
/// `(reached_running, reason)`: `reached_running` tells the caller whether
/// to reset the backoff ladder; `reason` is the human-readable cause of
/// this generation's end (spawn failure, the child's exit — before or
/// after hello alike — or a owner-dropped shutdown).
///
/// The binary is resolved FRESH here, not cached: a removed or replaced
/// juliaup install recovers on the very next attempt.
async fn run_one_generation(
    kernel_project: &Path,
    project_root: &Path,
    status: &watch::Sender<Status>,
) -> (bool, String) {
    let (julia_bin, source) = match crate::julia::resolve_bin() {
        Ok(v) => v,
        Err(reason) => return (false, reason),
    };
    if !kernel_project.exists() {
        return (false, format!("kernel project missing at {}", kernel_project.display()));
    }
    let project_root_str = match project_root.to_str() {
        Some(s) => s,
        None => return (false, format!("project_root not utf-8: {}", project_root.display())),
    };
    let escaped = project_root_str.replace('\\', "\\\\").replace('"', "\\\"");
    let julia_src = format!(
        "using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout; project_root=\"{escaped}\")"
    );

    tracing::info!(julia_bin = %julia_bin, source, "spawning kernel");

    let mut child: Child = match Command::new(&julia_bin)
        .arg(format!("--project={}", kernel_project.display()))
        .arg("-e")
        .arg(&julia_src)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Belt and braces: this task's own future should never be dropped
        // mid-generation (it isn't owned by any cancellable caller job —
        // see the module invariant), but if it ever were, the OS kills the
        // child the instant `child` drops instead of orphaning it.
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("spawn {julia_bin} failed: {e}")),
    };

    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => return (false, "kernel child stdin missing".to_string()),
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return (false, "kernel child stdout missing".to_string()),
    };
    let stderr = match child.stderr.take() {
        Some(s) => s,
        None => return (false, "kernel child stderr missing".to_string()),
    };

    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::debug!(target: "kernel.stderr", "{line}");
        }
    });

    let (submit_tx, mut submit_rx) = mpsc::channel::<Submission>(64);
    let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
    let mut next_id: u64 = 1;
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut published_running = false;

    // Submit `kernel.hello` to OURSELVES through the exact same
    // Submission/pending machinery every other op uses — one
    // response-reading path, not two.
    let (hello_tx, mut hello_rx) = oneshot::channel();
    let hello_id = next_id;
    next_id += 1;
    pending.insert(hello_id, hello_tx);
    if let Err(e) = write_request(&mut stdin, hello_id, "kernel.hello", &json!({})).await {
        return (false, format!("julia exited at once (path {julia_bin}): {e}"));
    }

    loop {
        tokio::select! {
            biased;
            // Every `Kernel` handle sharing this `status` has been dropped
            // (a destroyed workspace, most commonly) — stop serving; the
            // function returning drops `child` (`kill_on_drop`) and
            // `pending`'s senders (their receivers, if any caller is
            // somehow still awaiting one, just see a dropped channel —
            // nobody is watching `status` to read a `Dead` we could no
            // longer deliver anyway).
            _ = status.closed() => {
                return (published_running, "owning kernel handle dropped".to_string());
            }
            hello = &mut hello_rx, if !published_running => {
                match hello {
                    Ok(Ok(payload)) => {
                        log_hello(&payload);
                        published_running = true;
                        let _ = status.send(Status::Running(submit_tx.clone()));
                    }
                    Ok(Err(e)) => {
                        return (false, format!("julia exited at once (path {julia_bin}): {e:#}"));
                    }
                    Err(_) => {
                        // `pending`'s hello entry can only be consumed by
                        // `route_response` (success) or `finish_generation`
                        // (drained on death, which itself returns) — so a
                        // bare `RecvError` here is unreachable in practice.
                    }
                }
            }
            sub = submit_rx.recv() => {
                let Some(sub) = sub else {
                    // Unreachable while this function holds `submit_tx`
                    // itself (it does, for the whole loop) — kept as a
                    // defensive exit rather than an `unreachable!()`.
                    return (published_running, "kernel submission channel closed".to_string());
                };
                let id = next_id;
                next_id += 1;
                if let Err(e) = write_request(&mut stdin, id, &sub.op, &sub.payload).await {
                    let _ = sub.reply.send(Err(anyhow!("kernel stdin: {e}")));
                    let reason = format!("julia exited at once (path {julia_bin}): {e}");
                    return finish_generation(status, published_running, reason, pending).await;
                }
                pending.insert(id, sub.reply);
            }
            line = stdout_lines.next_line() => {
                match line {
                    Ok(Some(line)) => route_response(&line, &mut pending),
                    Ok(None) => {
                        tracing::warn!("kernel child stdout closed");
                        let reason = format!(
                            "julia exited {} (path {julia_bin})",
                            if published_running { "mid-request" } else { "at once" }
                        );
                        return finish_generation(status, published_running, reason, pending).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "kernel stdout error");
                        let reason = format!("kernel stdout error (path {julia_bin}): {e}");
                        return finish_generation(status, published_running, reason, pending).await;
                    }
                }
            }
        }
    }
}

/// Common death-handling tail, shared by every exit path out of
/// `run_one_generation`'s serving loop that ISN'T an owner-dropped
/// shutdown: publish `Dead` BEFORE draining `pending` — so a caller
/// re-checking status the instant a submission fails sees the SAME reason,
/// never a stale `Running` — then deliver the typed
/// `KernelUnavailable::Dead` to every affected in-flight submission (not a
/// generic "kernel terminated" string).
async fn finish_generation(
    status: &watch::Sender<Status>,
    published_running: bool,
    reason: String,
    mut pending: HashMap<u64, oneshot::Sender<Result<Value>>>,
) -> (bool, String) {
    let _ = status.send(Status::Dead { reason: reason.clone() });
    for (_id, reply) in pending.drain() {
        let _ = reply.send(Err(anyhow::Error::new(KernelUnavailable::Dead {
            reason: reason.clone(),
        })));
    }
    (published_running, reason)
}

async fn write_request(stdin: &mut ChildStdin, id: u64, op: &str, payload: &Value) -> Result<()> {
    let req = WireRequest { v: 1, id, kind: "req", op, payload };
    let mut line = serde_json::to_vec(&req).map_err(|e| anyhow!("kernel serialize: {e}"))?;
    line.push(b'\n');
    stdin.write_all(&line).await?;
    stdin.flush().await?;
    Ok(())
}

fn route_response(line: &str, pending: &mut HashMap<u64, oneshot::Sender<Result<Value>>>) {
    let parsed: Result<WireResponse, _> = serde_json::from_str(line);
    let resp = match parsed {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, line, "kernel response parse failed");
            return;
        }
    };
    let Some(reply) = pending.remove(&resp.id) else {
        tracing::debug!(id = resp.id, op = ?resp.op, "kernel response without pending request");
        return;
    };
    let _ = reply.send(Ok(resp.payload));
}

/// Log `kernel.hello`'s payload against `KERNEL_PROTOCOL_VERSION` (ADR 0030
/// §2). Non-fatal either way: a mismatch is ERROR (kernel keeps running —
/// BE + kernel ship as a unit, so a live-but-skewed kernel is still more
/// useful than a dead one), a missing `protocol` field (pre-ADR-0030
/// kernel) is WARN.
fn log_hello(payload: &Value) {
    let kernel_version = payload.get("version").and_then(|v| v.as_str()).unwrap_or("?");
    match payload.get("protocol").and_then(|v| v.as_u64()) {
        Some(p) if p as u32 == KERNEL_PROTOCOL_VERSION => {
            tracing::info!(kernel_protocol = p, %kernel_version, "kernel hello ok (protocol matches)");
        }
        Some(p) => {
            tracing::error!(
                kernel_protocol = p,
                expected = KERNEL_PROTOCOL_VERSION,
                %kernel_version,
                "kernel PROTOCOL_VERSION mismatch: backend expects {}, kernel reports {} \
                 — stale kernel checkout? (BE + kernel ship as a unit; NOT killing the \
                 kernel, see ADR 0030)",
                KERNEL_PROTOCOL_VERSION,
                p,
            );
        }
        None => {
            tracing::warn!(
                %kernel_version,
                "kernel hello omitted `protocol` — pre-ADR-0030 kernel; skipping the \
                 BE↔kernel protocol validation"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_unavailable_display_is_composable() {
        let dead = KernelUnavailable::Dead {
            reason: "julia exited at once (path /bin/false)".to_string(),
        };
        assert_eq!(
            format!("Julia kernel unavailable: {dead}"),
            "Julia kernel unavailable: julia exited at once (path /bin/false)"
        );
        let starting = KernelUnavailable::Starting { waited: Duration::from_secs(10) };
        assert!(format!("{starting}").contains("still starting"));
    }

    #[tokio::test]
    async fn missing_kernel_project_reports_dead_without_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let missing_project = dir.path().join("does-not-exist");
        let kernel = Kernel::new(missing_project, dir.path().to_path_buf());
        let err = kernel.request("kernel.hello", json!({})).await.unwrap_err();
        let unavailable = err
            .downcast_ref::<KernelUnavailable>()
            .unwrap_or_else(|| panic!("expected KernelUnavailable, got: {err:#}"));
        match unavailable {
            KernelUnavailable::Dead { reason } => {
                assert!(reason.contains("kernel project missing"), "unexpected reason: {reason}");
            }
            other => panic!("expected Dead, got {other:?}"),
        }
    }

    /// The leak fix: once every `Kernel` handle drops, the supervisor task
    /// must actually end (not run forever as an orphan holding its own
    /// strong reference). Exercised at the `watch` level directly —
    /// `supervisor_loop` itself is `async fn` and not otherwise observable
    /// from outside this module without a real child process.
    #[tokio::test]
    async fn status_channel_reports_closed_once_every_kernel_handle_drops() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = Kernel::new(dir.path().to_path_buf(), dir.path().to_path_buf());
        let status = kernel.inner.status.clone();
        assert!(!status.is_closed(), "keepalive receiver should hold it open");
        drop(kernel);
        assert!(status.is_closed(), "dropping the last Kernel handle should close status");
    }
}
