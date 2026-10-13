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
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, watch, OnceCell};

use sot_protocol::child_lines::ChildLines;

use super::WireRequest;

/// Bound on ONE caller's wait for a reply — covering both "the kernel is
/// still starting" and "the kernel is running but this op is slow" alike,
/// since a caller cannot tell those apart from the outside and shouldn't
/// need to: either way, it must not wait longer than this. A cold
/// `using`-time precompile can legitimately take well past this on a first
/// boot; the supervisor keeps the child running regardless (see the module
/// invariant above) so a LATER caller, once startup finishes, succeeds
/// immediately — this bound only ever costs one caller one message.
const KERNEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Respawn backoff floor: the first failed spawn, and the first failure
/// after a kernel that served `STABLE` after its hello, waits this long
/// before the supervisor tries again.
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

/// How long a generation waits on its child before it ends the generation: `start` for the hello (a cold precompile of
/// the kernel's own environment), `answer` once the hello has answered, counted from the later of the oldest request's
/// write and the kernel's last answer (the longest answer a working kernel gives, HDF5Preview's first load included).
#[derive(Clone, Copy)]
pub(crate) struct KernelBounds {
    pub(crate) start: Duration,
    pub(crate) answer: Duration,
}

impl KernelBounds {
    pub(crate) const PRODUCT: KernelBounds = KernelBounds {
        start: Duration::from_secs(1800),
        answer: Duration::from_secs(300),
    };
}

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
    /// A running kernel did not answer this request within the caller's deadline. The kernel is still running; the
    /// supervisor replaces it only once it has answered nothing for `KernelBounds::answer`.
    NoAnswer { op: String, waited: Duration },
}

impl std::fmt::Display for KernelUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelUnavailable::Dead { reason } => write!(f, "{reason}"),
            KernelUnavailable::Starting { waited } => {
                write!(f, "kernel still starting (no response after {waited:?})")
            }
            KernelUnavailable::NoAnswer { op, waited } => {
                write!(f, "the running kernel did not answer {op} within {waited:?}")
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
                tokio::spawn(supervisor_loop(
                    kernel_project,
                    project_root,
                    status,
                    crate::lifecycle::child_signal::process(),
                    KernelBounds::PRODUCT,
                ));
            })
            .await;
    }
}

/// Submit one request to an already-`Running` kernel and await its reply, the send included, bounded by `timeout`
/// (the CALLER's remaining budget, not a fresh one): a full channel, behind a supervisor stuck writing to a kernel that
/// reads nothing, counts against it.
async fn submit_and_await(
    tx: &mpsc::Sender<Submission>,
    op: &str,
    payload: Value,
    timeout: Duration,
) -> Result<Value> {
    let call = async {
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Submission {
            op: op.to_string(),
            payload,
            reply: reply_tx,
        })
        .await
        .map_err(|_| anyhow!("kernel supervisor channel closed"))?;
        reply_rx.await.map_err(|_| anyhow!("kernel supervisor dropped reply channel"))?
    };
    match tokio::time::timeout(timeout, call).await {
        Ok(r) => r,
        Err(_) => Err(anyhow::Error::new(KernelUnavailable::NoAnswer {
            op: op.to_string(),
            waited: KERNEL_REQUEST_TIMEOUT,
        })),
    }
}

/// The persistent loop: spawn a generation of the child, run it until it
/// answers hello and then dies (or dies before ever answering), record
/// `Dead` with the wait platform's `Redial` gives (started over only after
/// a kernel that served `STABLE` after its hello, so a kernel that never
/// answers, or answers and then dies, keeps the doubling however long its
/// precompile ran), sleep, repeat — until `status`
/// closes (every `Kernel` handle sharing it has been dropped), at which
/// point this returns and the task ends. It also ends when `sig` fires, and
/// never respawns after. A generation whose child answers nothing within
/// `bounds` is ended and replaced the same way.
async fn supervisor_loop(
    kernel_project: PathBuf,
    project_root: PathBuf,
    status: watch::Sender<Status>,
    sig: &'static crate::lifecycle::child_signal::Signal,
    bounds: KernelBounds,
) {
    let mut redial = sot_log::host::redial::Redial::new(RESPAWN_BACKOFF_FLOOR, RESPAWN_BACKOFF_CAP);
    loop {
        if status.is_closed() || sig.is_fired() {
            return;
        }
        let _ = status.send(Status::Starting);
        let (served_since, reason) = run_one_generation(&kernel_project, &project_root, &status, sig, bounds).await;
        if status.is_closed() || sig.is_fired() {
            return;
        }
        // A kernel's working life starts at its hello: a precompile that never answers counts as nothing.
        let wait = redial.after(served_since.map_or(Duration::ZERO, |t| t.elapsed()));
        tracing::warn!(reason = %reason, next_retry_in = ?wait, "kernel unavailable; will retry after backoff");
        let _ = status.send(Status::Dead { reason });
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = sig.fired() => return,
        }
    }
}

/// The program a generation spawns. Tests point one kernel project at a stub.
fn julia_bin(kernel_project: &Path) -> Result<(String, &'static str), String> {
    #[cfg(test)]
    if let Some((_, bin)) = tests::STUB_BIN.lock().unwrap().iter().find(|(p, _)| p == kernel_project) {
        return Ok((bin.clone(), "test stub"));
    }
    let _ = kernel_project;
    crate::sidecars::julia::resolve_bin()
}

/// Resolve + spawn one child, fold `kernel.hello` into the same
/// request/response loop every other op uses (no separate raw exchange),
/// publish `Running` the moment it answers, then keep serving until it
/// dies OR `status` closes (the owning `Kernel` was dropped). On every
/// return the child's tree is ended and reaped, and its temporary folder
/// removed (`ChildTmp::retire`). Returns when the kernel answered hello
/// (`None` if it never did), from which its working life is measured, and
/// the human-readable cause of this
/// generation's end (spawn failure, the child's exit — before or after
/// hello alike — or a owner-dropped shutdown).
///
/// The binary is resolved FRESH here, not cached: a removed or replaced
/// juliaup install recovers on the very next attempt.
async fn run_one_generation(
    kernel_project: &Path,
    project_root: &Path,
    status: &watch::Sender<Status>,
    sig: &'static crate::lifecycle::child_signal::Signal,
    bounds: KernelBounds,
) -> (Option<tokio::time::Instant>, String) {
    let (julia_bin, source) = match julia_bin(kernel_project) {
        Ok(v) => v,
        Err(reason) => return (None, reason),
    };
    if !kernel_project.exists() {
        return (None, format!("kernel project missing at {}", kernel_project.display()));
    }
    let project_root_str = match project_root.to_str() {
        Some(s) => s,
        None => return (None, format!("project_root not utf-8: {}", project_root.display())),
    };
    let escaped = project_root_str.replace('\\', "\\\\").replace('"', "\\\"");
    let julia_src = format!(
        "using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout; project_root=\"{escaped}\")"
    );

    tracing::info!(julia_bin = %julia_bin, source, "spawning kernel");

    let tmp = match crate::sidecars::child_tmp::ChildTmp::new() {
        Ok(tmp) => tmp,
        Err(e) => return (None, format!("kernel child temporary folder: {e}")),
    };
    let mut cmd = Command::new(&julia_bin);
    tmp.apply(&mut cmd);
    cmd.arg(format!("--project={}", kernel_project.display()))
        .arg("-e")
        .arg(&julia_src)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // `contained` is the one kill: it ends the tree before the child on every
    // return, and the shutdown fires it from anywhere, so a child this loop is
    // not polling for (a full stdin pipe) and what it started still die.
    let mut contained = match sig.spawn(&mut cmd) {
        Ok(c) => c,
        Err(e) => return (None, format!("spawn {julia_bin} failed: {e}")),
    };
    let outcome = serve_child(&mut contained, &julia_bin, status, sig, bounds).await;
    // The child is ended and reaped before its folder goes, however the generation ended.
    if let Err(e) = tmp.retire(&mut contained).await {
        tracing::warn!(error = %e, "kernel child cleanup failed");
    }
    outcome
}

/// Takes the child's pipes and sends its stderr to the log. Returns its stdin and stdout.
fn take_pipes(contained: &mut crate::lifecycle::child_signal::Contained) -> Result<(ChildStdin, ChildStdout), String> {
    let stdin = contained.stdin.take().ok_or("kernel child stdin missing")?;
    let stdout = contained.stdout.take().ok_or("kernel child stdout missing")?;
    let stderr = contained.stderr.take().ok_or("kernel child stderr missing")?;
    tokio::spawn(async move {
        let mut reader = ChildLines::new(stderr);
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::debug!(target: "kernel.stderr", "{line}");
        }
    });
    Ok((stdin, stdout))
}

/// One request in flight to the child: who waits for it, what it is, and when it was written.
struct InFlight {
    op: String,
    since: tokio::time::Instant,
    reply: oneshot::Sender<Result<Value>>,
}

impl InFlight {
    fn new(op: &str, reply: oneshot::Sender<Result<Value>>) -> Self {
        Self { op: op.to_string(), since: tokio::time::Instant::now(), reply }
    }
}

type Pending = HashMap<u64, InFlight>;

/// The oldest request in flight (ids rise with time).
fn oldest(pending: &Pending) -> Option<&InFlight> {
    pending.iter().min_by_key(|(id, _)| **id).map(|(_, in_flight)| in_flight)
}

/// When the generation must end for want of an answer. Before the hello has answered (`heard` is `None`): the oldest
/// request's write time plus `start`. After: the later of that write time and `heard`, the kernel's last answer, plus
/// `answer`, so a request queued behind slow answers is not counted as the kernel's silence. `None` while nothing is in
/// flight, or when the bound is too long to add.
fn next_due(pending: &Pending, heard: Option<tokio::time::Instant>, bounds: KernelBounds) -> Option<tokio::time::Instant> {
    let since = oldest(pending)?.since;
    match heard {
        Some(heard) => since.max(heard).checked_add(bounds.answer),
        None => since.checked_add(bounds.start),
    }
}

/// Why the generation ended for want of an answer.
fn late_reason(pending: &Pending, heard: Option<tokio::time::Instant>, bounds: KernelBounds) -> String {
    match oldest(pending) {
        Some(f) if heard.is_some() => format!("the kernel answered nothing for {:?} (oldest: {})", bounds.answer, f.op),
        _ => format!("the kernel did not answer its hello within {:?}", bounds.start),
    }
}

/// Resolves at `due`, never when there is none.
async fn until(due: Option<tokio::time::Instant>) {
    match due {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// How one request's write to the child ended: it finished, or the generation had to end first.
enum Written {
    Done(Result<()>),
    Shutdown,
    Dropped,
    Late,
}

/// `write_request` raced against the shutdown, the owner's drop and the oldest request's deadline, so a child that
/// reads nothing (a full pipe) cannot hold the generation.
async fn write_raced(
    stdin: &mut ChildStdin,
    (id, op, payload): (u64, &str, &Value),
    status: &watch::Sender<Status>,
    sig: &'static crate::lifecycle::child_signal::Signal,
    due: Option<tokio::time::Instant>,
) -> Written {
    tokio::select! {
        biased;
        _ = sig.fired() => Written::Shutdown,
        _ = status.closed() => Written::Dropped,
        _ = until(due) => Written::Late,
        r = write_request(stdin, id, op, payload) => Written::Done(r),
    }
}

/// One generation's serving loop, from the hello until the child dies, every handle drops, the daemon shuts down, or
/// the child answers nothing within `bounds`. `contained` stays with the caller, which ends it.
async fn serve_child(
    contained: &mut crate::lifecycle::child_signal::Contained,
    julia_bin: &str,
    status: &watch::Sender<Status>,
    sig: &'static crate::lifecycle::child_signal::Signal,
    bounds: KernelBounds,
) -> (Option<tokio::time::Instant>, String) {
    let (mut stdin, stdout) = match take_pipes(contained) {
        Ok(pipes) => pipes,
        Err(reason) => return (None, reason),
    };

    let (submit_tx, mut submit_rx) = mpsc::channel::<Submission>(64);
    let mut pending: Pending = HashMap::new();
    let mut next_id: u64 = 1;
    let mut stdout_lines = ChildLines::new(stdout);
    let mut served_since: Option<tokio::time::Instant> = None;
    // When the kernel last answered a request, its hello included: `None` until the hello's answer.
    let mut heard: Option<tokio::time::Instant> = None;

    // Submit `kernel.hello` to OURSELVES through the exact same
    // Submission/pending machinery every other op uses — one
    // response-reading path, not two.
    let (hello_tx, mut hello_rx) = oneshot::channel();
    let hello_id = next_id;
    next_id += 1;
    pending.insert(hello_id, InFlight::new("kernel.hello", hello_tx));
    let due = next_due(&pending, None, bounds);
    match write_raced(&mut stdin, (hello_id, "kernel.hello", &json!({})), status, sig, due).await {
        Written::Done(Ok(())) => {}
        Written::Done(Err(e)) => return (None, format!("julia exited at once (path {julia_bin}): {e}")),
        Written::Shutdown => return (None, "the daemon is shutting down".to_string()),
        Written::Dropped => return (None, "owning kernel handle dropped".to_string()),
        Written::Late => return (None, late_reason(&pending, None, bounds)),
    }

    loop {
        tokio::select! {
            biased;
            // The daemon is shutting down: the signal has already killed the
            // child's tree.
            _ = sig.fired() => {
                return (served_since, "the daemon is shutting down".to_string());
            }
            // Every `Kernel` handle sharing this `status` has been dropped
            // (a destroyed workspace, most commonly) — stop serving; the
            // generation's return ends the tree (`retire`), and
            // `pending`'s senders (their receivers, if any caller is
            // somehow still awaiting one, just see a dropped channel —
            // nobody is watching `status` to read a `Dead` we could no
            // longer deliver anyway).
            _ = status.closed() => {
                return (served_since, "owning kernel handle dropped".to_string());
            }
            hello = &mut hello_rx, if served_since.is_none() => {
                match hello {
                    Ok(Ok(payload)) => {
                        log_hello(&payload);
                        served_since = Some(tokio::time::Instant::now());
                        heard = served_since;
                        let _ = status.send(Status::Running(submit_tx.clone()));
                    }
                    Ok(Err(e)) => {
                        return (None, format!("julia exited at once (path {julia_bin}): {e:#}"));
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
                    return (served_since, "kernel submission channel closed".to_string());
                };
                let id = next_id;
                next_id += 1;
                pending.insert(id, InFlight::new(&sub.op, sub.reply));
                let due = next_due(&pending, heard, bounds);
                let reason = match write_raced(&mut stdin, (id, &sub.op, &sub.payload), status, sig, due).await {
                    Written::Done(Ok(())) => continue,
                    Written::Done(Err(e)) => {
                        if let Some(sent) = pending.remove(&id) {
                            let _ = sent.reply.send(Err(anyhow!("kernel stdin: {e}")));
                        }
                        format!("julia exited at once (path {julia_bin}): {e}")
                    }
                    Written::Shutdown => return (served_since, "the daemon is shutting down".to_string()),
                    Written::Dropped => return (served_since, "owning kernel handle dropped".to_string()),
                    Written::Late => late_reason(&pending, heard, bounds),
                };
                return (served_since, finish_generation(status, reason, pending).await);
            }
            line = stdout_lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if route_response(&line, &mut pending) && heard.is_some() {
                            heard = Some(tokio::time::Instant::now());
                        }
                    }
                    Ok(None) => {
                        tracing::warn!("kernel child stdout closed");
                        let reason = format!(
                            "julia exited {} (path {julia_bin})",
                            if served_since.is_some() { "mid-request" } else { "at once" }
                        );
                        return (served_since, finish_generation(status, reason, pending).await);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "kernel stdout error");
                        let reason = format!("kernel stdout error (path {julia_bin}): {e}");
                        return (served_since, finish_generation(status, reason, pending).await);
                    }
                }
            }
            // The child answered nothing for the bound: a wedged kernel is ended and replaced.
            _ = until(next_due(&pending, heard, bounds)) => {
                let reason = late_reason(&pending, heard, bounds);
                return (served_since, finish_generation(status, reason, pending).await);
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
async fn finish_generation(status: &watch::Sender<Status>, reason: String, mut pending: Pending) -> String {
    let _ = status.send(Status::Dead { reason: reason.clone() });
    for (_id, in_flight) in pending.drain() {
        let _ = in_flight.reply.send(Err(anyhow::Error::new(KernelUnavailable::Dead {
            reason: reason.clone(),
        })));
    }
    reason
}

async fn write_request(stdin: &mut ChildStdin, id: u64, op: &str, payload: &Value) -> Result<()> {
    let req = WireRequest { v: 1, id, kind: "req", op, payload };
    let mut line = serde_json::to_vec(&req).map_err(|e| anyhow!("kernel serialize: {e}"))?;
    line.push(b'\n');
    stdin.write_all(&line).await?;
    stdin.flush().await?;
    Ok(())
}

/// Hands an answer line to the request it answers; whether it answered one in flight.
fn route_response(line: &str, pending: &mut Pending) -> bool {
    let parsed: Result<WireResponse, _> = serde_json::from_str(line);
    let resp = match parsed {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %sot_protocol::codec::unparsed(&e, line.len()), "kernel response parse failed");
            return false;
        }
    };
    let Some(in_flight) = pending.remove(&resp.id) else {
        tracing::debug!(id = resp.id, op = ?resp.op, "kernel response without pending request");
        return false;
    };
    let _ = in_flight.reply.send(Ok(resp.payload));
    true
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
    #[cfg(unix)]
    use crate::lifecycle::child_signal::tests::Leftover;

    /// Program-path override per kernel project, so no test touches the process env.
    pub(super) static STUB_BIN: std::sync::Mutex<Vec<(PathBuf, String)>> = std::sync::Mutex::new(Vec::new());

    /// The request line the kernel child reads: fields in declaration order.
    #[test]
    fn wire_request_line_is_pinned() {
        let line = serde_json::to_string(&WireRequest {
            v: 1,
            id: 7,
            kind: "req",
            op: "x.y",
            payload: &serde_json::json!({"a": [1, "b"]}),
        })
        .unwrap();
        assert_eq!(line, r#"{"v":1,"id":7,"kind":"req","op":"x.y","payload":{"a":[1,"b"]}}"#);
    }

    /// The shutdown signal kills the kernel child and the loop neither
    /// respawns nor leaves a registered tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_kills_the_kernel_child_and_never_respawns() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("spawns");
        let stub = dir.path().join("stub-julia");
        sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\necho x >> {}\nexec sleep 30\n", counter.display()));
        let project = dir.path().join("kp");
        std::fs::create_dir(&project).unwrap();
        STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (status, _keep) = watch::channel(Status::Starting);
        let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
        let began = std::time::Instant::now();
        // Spawn completes before the stub's first line has run; fire only once
        // that line has written the counter, so the one-spawn precondition is true.
        while std::fs::read_to_string(&counter).map_or(0, |s| s.lines().count()) == 0 {
            assert!(began.elapsed() < Duration::from_secs(5), "the stub child never ran");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!task.is_finished(), "the ready owner finished before the fire");
        sig.fire().expect("fire");
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the supervisor loop outlived the shutdown")
            .expect("supervisor task");
        assert!(sig.held_groups().is_empty());
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(), 1, "respawned after the fire");
    }

    /// A kernel that stops reading fills its pipe and the supervisor sits in
    /// `write_request`, where it never polls the signal: the shutdown must
    /// still take the kernel and what the kernel started.
    #[cfg(unix)]
    #[tokio::test]
    async fn blocked_kernel_write_does_not_outlive_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let gc_file = dir.path().join("gc");
        let gc = Leftover::of_file(gc_file.clone());
        let stub = dir.path().join("stub-julia");
        sot_log::test_exec::write_executable(
            &stub,
            format!(
                "#!/bin/sh\nsleep 3101 &\necho $! > {}\nread l\necho '{{\"id\":1,\"payload\":{{\"protocol\":0}}}}'\nexec sleep 3101\n",
                gc_file.display()
            ),
        );
        let project = dir.path().join("kp");
        std::fs::create_dir(&project).unwrap();
        STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (status, mut status_rx) = watch::channel(Status::Starting);
        let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
        let tx = loop {
            let seen = status_rx.borrow_and_update().clone();
            if let Status::Running(tx) = seen {
                break tx;
            }
            tokio::time::timeout(Duration::from_secs(5), status_rx.changed())
                .await
                .expect("the stub kernel never answered hello")
                .expect("status channel");
        };
        // 256 KiB per request: the stub never reads again, so the first
        // write fills the pipe and the rest queue behind it.
        let big = "x".repeat(256 * 1024);
        let mut replies = Vec::new();
        for _ in 0..3 {
            let (reply, rx) = oneshot::channel();
            tx.send(Submission { op: "test.big".to_string(), payload: json!(big), reply }).await.unwrap();
            replies.push(rx);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(tx.capacity() < tx.max_capacity(), "the supervisor was not blocked on a full pipe");
        sig.fire().expect("fire");
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the supervisor loop outlived the shutdown")
            .expect("supervisor task");
        assert!(gc.gone(), "the kernel's grandchild survived the shutdown");
        assert!(sig.held_groups().is_empty());
        drop(replies);
    }

    /// A kernel that answers hello and then dies is respawned on the doubling wait, 250 ms to 30 s, not every 250 ms:
    /// an answered hello is not a working kernel (`supervisor_loop`, `Redial`). Each spawn appends a line; the doubling
    /// spawns at 0, 0.25, 0.75 and 1.75 s, then not before 3.75 s, and the waits are lower bounds, so a slow machine can
    /// only lower the count. Two spawns prove a respawn happened.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_kernel_that_answers_and_dies_is_respawned_on_the_doubling_wait() {
        const WINDOW: Duration = Duration::from_secs(3);
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("spawns");
        let stub = dir.path().join("stub-julia");
        sot_log::test_exec::write_executable(
            &stub,
            format!(
                "#!/bin/sh\necho x >> {}\nread l\necho '{{\"id\":1,\"payload\":{{\"protocol\":{KERNEL_PROTOCOL_VERSION}}}}}'\n",
                counter.display()
            ),
        );
        let project = dir.path().join("kp");
        std::fs::create_dir(&project).unwrap();
        STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (status, _keep) = watch::channel(Status::Starting);
        let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
        tokio::time::sleep(WINDOW).await;
        let spawns = std::fs::read_to_string(&counter).map_or(0, |s| s.lines().count());
        sig.fire().expect("fire");
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the supervisor loop outlived the shutdown")
            .expect("supervisor task");
        println!("respawn: {spawns} kernel spawns in {WINDOW:?} against a kernel that answers hello and exits");
        assert!(spawns >= 2, "the supervisor never respawned the kernel: {spawns} spawn(s) in {WINDOW:?}");
        assert!(spawns <= 4, "{spawns} kernel spawns in {WINDOW:?}: an answered hello restarted the wait");
    }

    /// A generation's temporary folder is removed once its child is ended and reaped. The stub stands in for the kernel
    /// on the generation's own spawn path: it writes a file into its temporary folder, answers hello and waits. The
    /// generation ends when its last handle drops, and then the owned folder that held the kernel's folder must be
    /// empty. `isolated` gives this process an owned temporary folder.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_generation_removes_its_temporary_folder_after_the_reap() {
        const BODY: Duration = Duration::from_secs(120);
        if !crate::sidecars::contract_tests::isolated(
            "sidecars::kernel::tests::a_generation_removes_its_temporary_folder_after_the_reap",
            BODY,
        ) {
            return;
        }
        let dir = tempfile::tempdir().expect("fixture folder");
        let owned = tempfile::tempdir().expect("owned temporary folder");
        std::env::set_var("TMPDIR", owned.path());
        let stub = dir.path().join("stub-julia");
        sot_log::test_exec::write_executable(
            &stub,
            format!(
                "#!/bin/sh\necho x > \"$TMPDIR/made\"\nread l\necho '{{\"id\":1,\"payload\":{{\"protocol\":{KERNEL_PROTOCOL_VERSION}}}}}'\nexec sleep 30\n"
            ),
        );
        let project = dir.path().join("kp");
        std::fs::create_dir(&project).unwrap();
        STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (status, mut keep) = watch::channel(Status::Starting);
        let root = dir.path().to_path_buf();
        let generation = tokio::spawn(async move { run_one_generation(&project, &root, &status, sig, KernelBounds::PRODUCT).await });
        tokio::time::timeout(Duration::from_secs(30), keep.wait_for(|s| matches!(s, Status::Running(_))))
            .await
            .expect("the stub answers hello")
            .expect("status open");
        let folders = || -> Vec<PathBuf> {
            std::fs::read_dir(owned.path())
                .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default()
        };
        let running = folders();
        assert!(
            running.len() == 1 && running[0].join("made").exists(),
            "setup: the kernel's folder is in the owned folder and holds the child's file: {running:?}"
        );
        drop(keep);
        let (_served, reason) = tokio::time::timeout(Duration::from_secs(30), generation)
            .await
            .expect("the generation ends when its handle drops")
            .expect("generation task");
        assert_eq!(reason, "owning kernel handle dropped");
        crate::sidecars::contract_tests::within(Duration::from_secs(30), "the kernel's folder is removed after its reap", || {
            folders().is_empty()
        })
        .await;
    }

    /// A kernel's working life starts at its hello. A precompile that runs past `STABLE` and then exits without
    /// answering, or answers and dies at once, is not a working kernel: the next respawn waits the doubling, 2 s after
    /// waits of 250 ms, 500 ms and 1 s, not the 250 ms floor. The paused clock moves only at the supervisor's waits and
    /// at the test's one sleep, which stands for the long precompile, so the gap between the fourth and fifth spawn is
    /// exact: that precompile plus the wait. A third case serves for `STABLE` after its hello and then dies: the next
    /// spawn comes at the 250 ms floor. Each stub blocks until the test releases it, so every `Starting` is seen;
    /// a watchdog thread fires the signal if a run stalls for a minute of real time.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_kernel_whose_precompile_outlasts_stable_without_serving_backs_off() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let precompile = sot_log::host::redial::STABLE + Duration::from_secs(1);
        for (late_hello, served) in [(false, false), (true, false), (false, true)] {
            let dir = tempfile::tempdir().unwrap();
            let counter = dir.path().join("spawns");
            let stub = dir.path().join("stub-julia");
            sot_log::test_exec::write_executable(
                &stub,
                format!(
                    "#!/bin/sh\necho x >> {c}\nn=$(wc -l < {c} | tr -d ' ')\nwhile [ ! -f {d}/go-$n ]; do sleep 0.02; done\nif [ -f {d}/hello-$n ]; then read l; echo '{{\"id\":1,\"payload\":{{\"protocol\":{KERNEL_PROTOCOL_VERSION}}}}}'; fi\nif [ -f {d}/hold-$n ]; then while [ ! -f {d}/die-$n ]; do sleep 0.02; done; fi\n",
                    c = counter.display(),
                    d = dir.path().display(),
                ),
            );
            let project = dir.path().join("kp");
            std::fs::create_dir(&project).unwrap();
            STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
            let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
            let (status, mut status_rx) = watch::channel(Status::Starting);
            // No deadline: a paused clock jumps to any armed timer whenever the runtime waits on the stub, which
            // would end each generation at its hello bound and measure nothing here.
            let unbounded = KernelBounds { start: Duration::MAX, answer: Duration::MAX };
            let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, unbounded));
            let done = std::sync::Arc::new(AtomicBool::new(false));
            let watchdog = {
                let done = std::sync::Arc::clone(&done);
                std::thread::spawn(move || {
                    for _ in 0..600 {
                        if done.load(Ordering::SeqCst) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    sig.fire().expect("the watchdog fires");
                })
            };
            let mut starts = Vec::new();
            let mut died = None;
            while starts.len() < 5 {
                status_rx.changed().await.expect("the supervisor ended before its fifth spawn (the watchdog fired)");
                if !matches!(*status_rx.borrow_and_update(), Status::Starting) {
                    continue;
                }
                starts.push(tokio::time::Instant::now());
                let n = starts.len();
                if n == 4 && served {
                    // The kernel answers at once and serves for `STABLE` and a second, then dies.
                    for f in ["hold-4", "hello-4", "go-4"] {
                        std::fs::write(dir.path().join(f), "").unwrap();
                    }
                    while !matches!(*status_rx.borrow_and_update(), Status::Running(_)) {
                        status_rx.changed().await.expect("the supervisor ended before the kernel answered hello");
                    }
                    tokio::time::sleep(precompile).await;
                    died = Some(tokio::time::Instant::now());
                    std::fs::write(dir.path().join("die-4"), "").unwrap();
                } else if n == 4 {
                    tokio::time::sleep(precompile).await;
                    if late_hello {
                        std::fs::write(dir.path().join("hello-4"), "").unwrap();
                    }
                }
                if n < 5 {
                    std::fs::write(dir.path().join(format!("go-{n}")), "").unwrap();
                }
            }
            done.store(true, Ordering::SeqCst);
            sig.fire().expect("fire");
            watchdog.join().unwrap();
            task.await.expect("supervisor task");
            if let Some(died) = died {
                let gap = starts[4] - died;
                println!("respawn: served for {precompile:?} after its hello: the spawn came {gap:?} after the kernel died");
                assert!(
                    gap <= Duration::from_millis(300),
                    "the spawn after a kernel that served {precompile:?} came {gap:?} after it died: the wait did not start over at the floor"
                );
                continue;
            }
            let gap = starts[4] - starts[3];
            println!("respawn: late_hello={late_hello}: the spawn after a {precompile:?} precompile came {gap:?} after it began");
            assert!(
                gap >= precompile + Duration::from_secs(2),
                "late_hello={late_hello}: the spawn after a {precompile:?} precompile came {gap:?} after it began: a kernel that did not serve counted as stable"
            );
        }
    }

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
        // A stand-in program, never spawned: the check under test comes after the julia lookup, and a host with
        // no julia (a CI runner) fails that lookup first.
        STUB_BIN.lock().unwrap().push((missing_project.clone(), "never-spawned".to_string()));
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

    /// ADR 0049, User isolation: a kernel line that does not parse is logged by position and length, never its bytes.
    #[test]
    fn a_kernel_line_that_does_not_parse_is_logged_without_its_bytes() {
        let logged = crate::sidecars::logged_by(|| {
            let mut pending = HashMap::new();
            route_response(r#"{"v":1,"id":2,"url":"http://127.0.0.1:41234/0123456789ab"#, &mut pending);
        });
        assert_eq!(logged.lines().filter(|l| l.contains("WARN")).count(), 1, "{logged}");
        assert!(!logged.contains("0123") && !logged.contains("41234"), "{logged}");
    }
}

/// The kernel's deadlines: a running kernel's silence is named as such, a kernel that answers nothing is ended and
/// replaced, and a request's send counts against its caller's bound.
#[cfg(all(test, unix))]
mod deadline_tests {
    use super::*;

    /// The lines of a stand-in kernel that read the hello and answer it.
    pub(super) fn hello_answer() -> String {
        format!("read l\necho '{{\"id\":1,\"payload\":{{\"protocol\":{KERNEL_PROTOCOL_VERSION}}}}}'\n")
    }

    /// A kernel project whose stand-in appends a line to the returned counter at each spawn and then runs `script`.
    pub(super) fn stub_kernel(dir: &Path, name: &str, script: &str) -> (PathBuf, PathBuf) {
        let counter = dir.join(format!("{name}-spawns"));
        let stub = dir.join(format!("{name}-julia"));
        sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\necho x >> {}\n{script}", counter.display()));
        let project = dir.join(name);
        std::fs::create_dir(&project).unwrap();
        tests::STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
        (project, counter)
    }

    /// A kernel project whose stand-in answers its hello (or not) and then reads and answers nothing.
    fn wedged_kernel(dir: &Path, name: &str, hello: bool) -> (PathBuf, PathBuf) {
        let answer = if hello { hello_answer() } else { String::new() };
        stub_kernel(dir, name, &format!("{answer}exec sleep 600\n"))
    }

    /// The running kernel's submission channel, once its stand-in has answered the hello (each wait within 5 s).
    pub(super) async fn running(status_rx: &mut watch::Receiver<Status>) -> mpsc::Sender<Submission> {
        loop {
            if let Status::Running(tx) = status_rx.borrow_and_update().clone() {
                return tx;
            }
            tokio::time::timeout(Duration::from_secs(5), status_rx.changed())
                .await
                .expect("the stub kernel never answered hello")
                .expect("status channel");
        }
    }

    pub(super) fn spawns(counter: &Path) -> usize {
        std::fs::read_to_string(counter).map_or(0, |s| s.lines().count())
    }

    /// Polls `condition` for up to `bound`.
    async fn becomes(bound: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let until = Instant::now() + bound;
        while !condition() {
            if Instant::now() >= until {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        true
    }

    /// A caller's timeout on a running kernel does not say the kernel is starting, and a kernel that answered its
    /// hello and then nothing is ended and started again once it has answered nothing for the answer bound.
    #[tokio::test]
    async fn a_wedged_kernel_is_named_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let (project, counter) = wedged_kernel(dir.path(), "named", true);
        let kernel = Kernel::new(project, dir.path().to_path_buf());
        let err = kernel.request("project.scan", json!({})).await.expect_err("a kernel that answers nothing");
        let named = format!("{err:#}");
        drop(kernel);

        let (project, counter2) = wedged_kernel(dir.path(), "replaced", true);
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (status, mut status_rx) = watch::channel(Status::Starting);
        let bounds = KernelBounds { start: Duration::from_secs(1800), answer: Duration::from_secs(2) };
        let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, bounds));
        let tx = running(&mut status_rx).await;
        let reason = submit_and_await(&tx, "project.scan", json!({}), KERNEL_REQUEST_TIMEOUT).await.expect_err("a kernel that answers nothing");
        let replaced = becomes(Duration::from_secs(15), || spawns(&counter2) >= 2).await;
        println!("kernel-answer: {reason:#}; spawned {} times", spawns(&counter2));
        sig.fire().expect("fire");
        tokio::time::timeout(Duration::from_secs(3), task).await.expect("the supervisor loop outlived the shutdown").expect("task");
        assert_eq!(spawns(&counter), 1);
        assert!(!named.contains("still starting"), "a running kernel's silence is named as starting: {named}");
        assert!(named.contains("the running kernel did not answer project.scan within 10s"), "{named}");
        assert!(replaced, "a kernel that answered nothing was never replaced: spawned {} time(s) in 15 s", spawns(&counter2));
        assert!(format!("{reason:#}").contains("answered nothing for 2s (oldest: project.scan)"), "{reason:#}");
    }

    /// A kernel that answers every request, each 2 s after the one before, is kept under a 3 s answer bound: the bound
    /// counts from the later of the oldest request's write and the kernel's last answer, so the time a request waits
    /// behind slow answers is not the kernel's silence.
    #[tokio::test]
    async fn a_kernel_answering_slowly_but_steadily_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let steady = r#"while read l; do sleep 2; id=$(printf '%s\n' "$l" | sed 's/.*"id":\([0-9]*\).*/\1/'); echo "{\"id\":$id,\"payload\":{}}"; done
"#;
        let (project, counter) = stub_kernel(dir.path(), "steady", &format!("{}{steady}", hello_answer()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (status, mut status_rx) = watch::channel(Status::Starting);
        let bounds = KernelBounds { start: Duration::from_secs(1800), answer: Duration::from_secs(3) };
        let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, bounds));
        let tx = running(&mut status_rx).await;
        let calls: Vec<_> = (1..=3)
            .map(|n| {
                let tx = tx.clone();
                tokio::spawn(async move { (n, submit_and_await(&tx, "test.slow", json!({}), Duration::from_secs(15)).await) })
            })
            .collect();
        let mut failed = Vec::new();
        for call in calls {
            let (n, answer) = call.await.expect("request task");
            if let Err(e) = answer {
                failed.push(format!("request {n}: {e:#}"));
            }
        }
        println!("kernel-steady: failed {failed:?}; spawned {} time(s)", spawns(&counter));
        sig.fire().expect("fire");
        tokio::time::timeout(Duration::from_secs(3), task).await.expect("the supervisor loop outlived the shutdown").expect("task");
        assert!(failed.is_empty(), "a kernel answering every 2 s was cut: {failed:?}");
        assert_eq!(spawns(&counter), 1, "a kernel answering every 2 s was replaced");
    }

    /// A kernel that never answers its hello within `start` is ended and replaced, and the reason names the hello.
    #[tokio::test]
    async fn a_kernel_that_never_says_hello_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let (project, counter) = wedged_kernel(dir.path(), "mute", false);
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (status, mut status_rx) = watch::channel(Status::Starting);
        let bounds = KernelBounds { start: Duration::from_secs(1), answer: Duration::from_secs(300) };
        let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, bounds));
        let reason = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                status_rx.changed().await.expect("status channel");
                if let Status::Dead { reason } = &*status_rx.borrow_and_update() {
                    break reason.clone();
                }
            }
        })
        .await
        .expect("a mute kernel was never ended");
        assert_eq!(reason, "the kernel did not answer its hello within 1s");
        assert!(becomes(Duration::from_secs(10), || spawns(&counter) >= 2).await, "a mute kernel was never replaced");
        sig.fire().expect("fire");
        tokio::time::timeout(Duration::from_secs(3), task).await.expect("the supervisor loop outlived the shutdown").expect("task");
    }

    /// A kernel that reads nothing fills its pipe, the supervisor sits in its write and the channel fills behind
    /// it: every request still ends within the caller's bound, the send included.
    #[tokio::test]
    async fn a_full_channel_still_answers_within_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let (project, _counter) = wedged_kernel(dir.path(), "full", true);
        let kernel = Kernel::new(project, dir.path().to_path_buf());
        let big = "x".repeat(64 * 1024);
        let mut calls = Vec::new();
        for n in 0..70 {
            let (kernel, big) = (kernel.clone(), big.clone());
            calls.push(tokio::spawn(async move {
                let began = Instant::now();
                let answered = tokio::time::timeout(Duration::from_secs(15), kernel.request("test.big", json!(big))).await.is_ok();
                (n, answered, began.elapsed())
            }));
        }
        let mut slowest = Duration::ZERO;
        for call in calls {
            let (n, answered, took) = call.await.expect("request task");
            assert!(answered, "request {n} waited 15 s on a kernel that reads nothing");
            slowest = slowest.max(took);
        }
        println!("kernel-full: 70 requests answered; the slowest after {slowest:?}");
        assert!(slowest < Duration::from_secs(11), "the slowest request took {slowest:?}");
        drop(kernel);
    }
}

/// A kernel's line may hold any byte: its replies and its stderr are read lossily (`ChildLines`), so one byte neither
/// ends the generation nor stops the stderr drain.
#[cfg(all(test, unix))]
mod stream_tests {
    use super::deadline_tests::{hello_answer, running, spawns, stub_kernel};
    use super::*;

    /// Runs a stand-in kernel that answers its hello and then runs `body`, sends it two `file.parse` requests (ids 2
    /// and 3), and returns their answers and how many times it was spawned.
    async fn two_answers(body: &str) -> (Vec<std::result::Result<Value, String>>, usize) {
        let dir = tempfile::tempdir().unwrap();
        let (project, counter) = stub_kernel(dir.path(), "bytes", &format!("{}{body}exec sleep 600\n", hello_answer()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (status, mut status_rx) = watch::channel(Status::Starting);
        let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
        let tx = running(&mut status_rx).await;
        let mut answers = Vec::new();
        for _ in 0..2 {
            let answer = submit_and_await(&tx, "file.parse", json!({}), KERNEL_REQUEST_TIMEOUT).await;
            answers.push(answer.map_err(|e| format!("{e:#}")));
        }
        sig.fire().expect("fire");
        tokio::time::timeout(Duration::from_secs(3), task).await.expect("the supervisor loop outlived the shutdown").expect("task");
        (answers, spawns(&counter))
    }

    /// A reply may hold any byte (JSON3 writes a string's bytes as they are, and a parse error quotes its source): it
    /// arrives with U+FFFD for the byte, and the same generation answers the next request.
    #[tokio::test]
    async fn a_kernel_reply_holding_an_invalid_byte_keeps_its_generation() {
        let (answers, spawns) = two_answers(
            "read l\nprintf '{\"id\":2,\"payload\":{\"error\":\"a\\377b\"}}\\n'\nread l\necho '{\"id\":3,\"payload\":{\"ok\":true}}'\n",
        )
        .await;
        assert_eq!((answers, spawns), (vec![Ok(json!({"error": "a\u{fffd}b"})), Ok(json!({"ok": true}))], 1));
    }

    /// A stderr line may hold any byte too: the drain reads on past it, so the kernel's later stderr (more than a pipe
    /// holds) neither waits for a reader nor fails its write.
    #[tokio::test]
    async fn a_kernel_stderr_line_holding_an_invalid_byte_keeps_its_generation() {
        let (answers, spawns) = two_answers(
            "read l\nprintf 'warn a\\377b\\n' >&2\ni=0\nwhile [ $i -lt 2048 ]; do echo 'more of the stderr, about sixty-four bytes on every line of it.' >&2 || exit 3; i=$((i + 1)); done\necho '{\"id\":2,\"payload\":{\"ok\":true}}'\nread l\necho '{\"id\":3,\"payload\":{\"ok\":true}}'\n",
        )
        .await;
        assert_eq!((answers, spawns), (vec![Ok(json!({"ok": true})), Ok(json!({"ok": true}))], 1));
    }
}
