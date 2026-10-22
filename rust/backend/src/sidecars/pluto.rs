// pluto.rs — supervisor for the Pluto-notebook sidecar.
//
// One shared Pluto server per backend, lazy-spawned on the first
// `pluto.open` call and re-used across calls. The Julia child runs
// `julia --project=<repo>/julia/pluto <repo>/julia/pluto/start.jl`;
// the script starts a `Pluto.ServerSession` on 127.0.0.1 (access-secret
// required, no auto-browser — security review: this is otherwise a second
// unauthenticated RCE on a shared host), prints `READY <base_url>` once the
// HTTP listener is bound, then services `OPEN <abspath>` requests on
// stdin, replying with `URL <url>` or `ERR <msg>` per line on stdout. The
// `URL` already carries `?secret=...` — Julia holds `session.secret` and
// builds the full URL itself. This supervisor relays that URL in the
// `pluto.open` reply and never logs it; the log writer masks any secret
// (`sot_log::secret`).
//
// Same lazy-respawn shape as mathjax.rs: on child death the
// supervisor drops the submission channel; the next caller respawns.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch, Mutex};

use crate::sidecars::child::{Start, StderrTail};

#[derive(Clone)]
pub struct Pluto {
    inner: Arc<PlutoInner>,
}

struct PlutoInner {
    project_dir: PathBuf,
    start_script: PathBuf,
    submit: Mutex<Option<Running>>,
}

/// The supervisor of one running server: submissions go in on `submit`, `ready` says where its start stands, `start`
/// reads its age and stderr while it is starting, and dropping `_ends` ends the server. The start belongs to the
/// supervisor, not to the open that began it, so an open that stops waiting leaves it going and the next joins it.
struct Running {
    submit: mpsc::Sender<Submission>,
    ready: watch::Receiver<Ready>,
    start: Start,
    _ends: oneshot::Sender<()>,
}

/// Where a server's start stands: waiting for `READY`, serving, or ended with the reason (the child is ended first).
#[derive(Clone, PartialEq)]
enum Ready {
    Starting,
    Up,
    Failed(String),
}

/// An open's answer: the notebook's URL, or, while the server it needs has not said `READY` within
/// `PLUTO_START_WAIT`, an account of the start (`Start::still_starting`). The second is no failure: the start goes on,
/// and the next open joins it.
#[derive(Debug, PartialEq)]
pub(crate) enum Opened {
    Url(String),
    Starting(String),
}

/// How long one open waits for a server that is still starting before it answers `Opened::Starting`: a warm start is
/// seconds and a cold precompile tens of seconds, so a stall is named within the `repl.execute` default wait.
const PLUTO_START_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long one open may take, from its submission to its `URL` or `ERR` line (Pluto loads the notebook before it
/// answers). An open past it is an error, and its server is ended: replies are routed oldest-first, so a late `URL`
/// would answer the next request.
const PLUTO_OPEN_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a start waits for the server's `READY` line before its supervisor ends the child. A first start after a Julia, package
/// or depot change precompiles Pluto and its dependencies, the same cold precompile the kernel's start bound allows
/// for, so it is that bound, `KernelBounds::PRODUCT.start`.
const PLUTO_READY_BOUND: std::time::Duration = super::kernel::KernelBounds::PRODUCT.start;

struct Submission {
    abs_path: String,
    reply: oneshot::Sender<Result<String>>,
}

impl Pluto {
    pub fn new(project_dir: PathBuf, start_script: PathBuf) -> Self {
        Self {
            inner: Arc::new(PlutoInner {
                project_dir,
                start_script,
                submit: Mutex::new(None),
            }),
        }
    }

    /// `julia/pluto` resolved for both layouts (dev checkout / release
    /// install) via `paths::resource_dir` (ADR 0030 §4).
    pub fn default_project_dir() -> PathBuf {
        sot_platform::host::paths::resource_dir("julia/pluto")
    }

    /// `<repo>/julia/pluto/start.jl` resolved via `CARGO_MANIFEST_DIR`.
    pub fn default_start_script() -> PathBuf {
        let mut p = Self::default_project_dir();
        p.push("start.jl");
        p
    }

    pub(crate) async fn open_notebook(&self, abs_path: &Path) -> Result<Opened> {
        let path_str = abs_path
            .to_str()
            .ok_or_else(|| anyhow!("pluto: path not utf-8: {}", abs_path.display()))?
            .to_string();
        let (tx, mut ready, start) = self.ensure_supervisor().await?;
        let state = tokio::time::timeout(PLUTO_START_WAIT, ready.wait_for(|state| *state != Ready::Starting)).await;
        match state.map(|state| state.map(|state| state.clone())) {
            Ok(Ok(Ready::Failed(why))) => {
                // The failed start has read its stderr to its end (`await_ready`), so these are the child's last words.
                return Err(match start.last_lines() {
                    said if said.is_empty() => anyhow!(why),
                    said => anyhow!("{why}; its last stderr lines: {said}"),
                });
            }
            Ok(Err(_)) => return Err(anyhow!("pluto sidecar ended before READY")),
            Ok(Ok(_)) => {}
            Err(_) => {
                let said = start.still_starting("Pluto");
                tracing::warn!("pluto.open of {path_str}: {said}");
                return Ok(Opened::Starting(said));
            }
        }
        let opened = tokio::time::timeout(PLUTO_OPEN_BOUND, async {
            let (reply_tx, reply_rx) = oneshot::channel();
            tx.send(Submission {
                abs_path: path_str.clone(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow!("pluto supervisor channel closed"))?;
            reply_rx
                .await
                .map_err(|_| anyhow!("pluto supervisor dropped reply channel"))?
        })
        .await;
        if let Ok(result) = opened {
            return result.map(Opened::Url);
        }
        // A server that does not answer within the bound is ended, unless a later open has already replaced it.
        let mut guard = self.inner.submit.lock().await;
        if guard.as_ref().is_some_and(|running| running.submit.same_channel(&tx)) {
            *guard = None;
        }
        Err(anyhow!(
            "pluto did not answer the open of {path_str} within {}s; its server was ended",
            PLUTO_OPEN_BOUND.as_secs()
        ))
    }

    /// Whether the Pluto server is running (started and not ended): ending the daemon ends it and every notebook open in it.
    pub(crate) async fn server_up(&self) -> bool {
        matches!(self.inner.submit.lock().await.as_ref(), Some(r) if !r.submit.is_closed() && !matches!(*r.ready.borrow(), Ready::Failed(_)))
    }

    async fn ensure_supervisor(&self) -> Result<(mpsc::Sender<Submission>, watch::Receiver<Ready>, Start)> {
        let mut guard = self.inner.submit.lock().await;
        if let Some(running) = guard.as_ref() {
            // A start that has failed is replaced even before its supervisor has dropped the queue.
            let failed = matches!(*running.ready.borrow(), Ready::Failed(_));
            if !running.submit.is_closed() && !failed {
                return Ok((running.submit.clone(), running.ready.clone(), running.start.clone()));
            }
        }
        let (julia_bin, _) = crate::sidecars::julia::resolve_bin().map_err(|e| anyhow!(e.to_string()))?;
        let running = spawn_supervisor(
            &julia_bin,
            &self.inner.project_dir,
            &self.inner.start_script,
            sot_containment::process(),
        )
        .await?;
        let handles = (running.submit.clone(), running.ready.clone(), running.start.clone());
        *guard = Some(running);
        Ok(handles)
    }
}

async fn spawn_supervisor(
    julia_bin: &str,
    project_dir: &Path,
    start_script: &Path,
    sig: &'static sot_containment::Signal,
) -> Result<Running> {
    if !start_script.exists() {
        return Err(anyhow!(
            "pluto start script missing at {}",
            start_script.display()
        ));
    }
    let tmp = crate::sidecars::child::ChildTmp::for_user_code(sig).await.context("pluto child temporary folder")?;
    let mut cmd = Command::new(julia_bin);
    tmp.apply(&mut cmd);
    cmd.arg(format!("--project={}", project_dir.display()))
        .arg(start_script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Counted and contained from spawn; the supervisor task owns it from here, its start included.
    let mut contained = sig
        .spawn(&mut cmd)
        .with_context(|| format!("spawn {julia_bin} --project={}", project_dir.display()))?;

    let stdin = contained.stdin.take().context("pluto child stdin missing")?;
    let stdout = contained.stdout.take().context("pluto child stdout missing")?;
    let stderr = contained.stderr.take().context("pluto child stderr missing")?;

    let stderr = StderrTail::drain(stderr, |line| tracing::debug!(target: "pluto.stderr", "{line}"));
    let start = stderr.start();
    let stdout_lines = sot_protocol::child_lines::ChildLines::new(stdout);

    let (submit_tx, submit_rx) = mpsc::channel::<Submission>(64);
    let (ends, ended) = oneshot::channel();
    let (ready_tx, ready) = watch::channel(Ready::Starting);
    tokio::spawn(run_supervisor(contained, stdin, stdout_lines, submit_rx, ended, sig, tmp, stderr, ready_tx));
    Ok(Running { submit: submit_tx, ready, start, _ends: ends })
}

/// The server's whole life: its start, published on `ready`, then its service loop.
async fn run_supervisor(
    mut contained: sot_containment::Contained,
    stdin: ChildStdin,
    mut stdout_lines: sot_protocol::child_lines::ChildLines<tokio::process::ChildStdout>,
    submit_rx: mpsc::Receiver<Submission>,
    mut ended: oneshot::Receiver<()>,
    sig: &'static sot_containment::Signal,
    tmp: crate::sidecars::child::ChildTmp,
    stderr: StderrTail,
    ready: watch::Sender<Ready>,
) {
    let (base_url, tmp, stderr) = match await_ready(&mut contained, &mut stdout_lines, tmp, stderr, sig, &mut ended).await {
        Ok(started) => started,
        Err(e) => {
            ready.send_replace(Ready::Failed(format!("{e:#}")));
            return;
        }
    };
    tracing::info!(base_url = %base_url, "pluto sidecar ready");
    // Publish the port the sidecar ACTUALLY bound (start.jl falls back to an
    // ephemeral port when 1234 is taken — another user's Pluto on a shared
    // host), through the one loopback parser. The ADR-0035 proxy allowlist
    // reads it so it authorizes the real server, never a stranger's process
    // squatting the preferred port. The grant belongs to this supervisor: the
    // guard moves into its task and is released on every way out.
    let grant = match sot_protocol::page_url::loopback_port_from_url(&base_url) {
        Some(port) => Some(Grant::publish(port)),
        None => {
            tracing::warn!(base_url = %base_url, "pluto READY url is not a loopback page address — proxy allowlist won't include pluto");
            None
        }
    };
    ready.send_replace(Ready::Up);
    supervisor_task(contained, stdin, stdout_lines, submit_rx, ended, sig, grant, tmp, stderr).await;
}

/// Waits for the `READY <base_url>` line, up to `PLUTO_READY_BOUND`: the supervisor can't usefully service OPEN
/// requests until the server is actually listening. A start that fails has its child ended first, and its stderr logged
/// at warn unless the daemon's shutdown or the handle ended it.
async fn await_ready(
    contained: &mut sot_containment::Contained,
    stdout_lines: &mut sot_protocol::child_lines::ChildLines<tokio::process::ChildStdout>,
    mut tmp: crate::sidecars::child::ChildTmp,
    stderr: StderrTail,
    sig: &'static sot_containment::Signal,
    ended: &mut oneshot::Receiver<()>,
) -> Result<(String, crate::sidecars::child::ChildTmp, StderrTail)> {
    let ready_deadline = tokio::time::Instant::now() + PLUTO_READY_BOUND;
    let late = || anyhow!("pluto sidecar did not emit READY within {}s", PLUTO_READY_BOUND.as_secs());
    let ready = loop {
        let now = tokio::time::Instant::now();
        if now >= ready_deadline {
            break Err(late());
        }
        let remaining = ready_deadline - now;
        let line = tokio::select! {
            line = tokio::time::timeout(remaining, stdout_lines.next_line()) => line,
            // The daemon is shutting down: the signal has already killed the
            // child's tree.
            _ = sig.fired() => {
                let _ = tmp.retire(contained).await;
                return Err(anyhow!("the daemon is shutting down"));
            }
            // Emptying the slot (the handle's drop) ends the child.
            _ = &mut *ended => {
                let _ = tmp.retire(contained).await;
                return Err(anyhow!("its handle ended it"));
            }
            _ = tmp.display_ended() => {
                break Err(anyhow!("pluto's display ended before READY, so the server was ended with it"));
            }
        };
        match line {
            Ok(Ok(Some(line))) => {
                if let Some(rest) = line.strip_prefix("READY ") {
                    break Ok(rest.trim().to_string());
                } else {
                    tracing::warn!(line = %line, "pluto sidecar pre-READY chatter");
                }
            }
            Ok(Ok(None)) => break Err(anyhow!("pluto sidecar stdout closed before READY")),
            Ok(Err(e)) => break Err(anyhow!("pluto sidecar stdout error: {e}")),
            Err(_) => break Err(late()),
        }
    };
    match ready {
        Ok(base_url) => Ok((base_url, tmp, stderr)),
        Err(e) => {
            let _ = tmp.retire(contained).await;
            stderr.warn(&format!("{e:#}")).await;
            Err(e)
        }
    }
}

/// The proxy grant of the current Pluto supervisor: `(generation, port)`, set by [`Grant::publish`] and cleared by
/// the drop of the same generation's guard, so a stale guard never erases a replacement's grant.
static GRANT: std::sync::Mutex<Option<(u64, u16)>> = std::sync::Mutex::new(None);
static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// One supervisor's hold on the proxy allowlist. Dropping it releases the port at once, whichever way the
/// supervisor ends.
pub(crate) struct Grant {
    generation: u64,
}

impl Grant {
    pub(crate) fn publish(port: u16) -> Grant {
        let generation = NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *GRANT.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, port));
        Grant { generation }
    }
}

impl Drop for Grant {
    fn drop(&mut self) {
        let mut current = GRANT.lock().unwrap_or_else(|e| e.into_inner());
        if current.is_some_and(|(generation, _)| generation == self.generation) {
            *current = None;
        }
    }
}

/// The port the current Pluto supervisor published after a loopback READY line, if it still holds its grant. The
/// proxy allowlist reads this instead of assuming the preferred 1234.
pub fn bound_pluto_port() -> Option<u16> {
    GRANT.lock().unwrap_or_else(|e| e.into_inner()).map(|(_, port)| port)
}

/// Why a supervisor stopped serving.
enum Cause {
    Signal,
    Exited(std::io::Result<()>),
    Closed,
    Released,
    Stdout(String),
    Stdin(std::io::Error),
    /// Its own display ended under it (Linux): a server left on an ended display could open the display's name once
    /// another account binds it.
    Display,
}

impl Cause {
    fn describe(&self) -> String {
        match self {
            Cause::Signal => "the daemon is shutting down".to_string(),
            Cause::Exited(Ok(())) => "the child exited".to_string(),
            Cause::Exited(Err(e)) => format!("the child's exit could not be observed: {e}"),
            Cause::Closed => "the owner dropped the sidecar".to_string(),
            Cause::Released => "its handle ended it".to_string(),
            Cause::Stdout(why) => format!("stdout: {why}"),
            Cause::Stdin(e) => format!("stdin: {e}"),
            Cause::Display => "its display ended, so the server was ended with it; the next open starts a new one".to_string(),
        }
    }
}

/// Answer the oldest pending request with one stdout line, if the line is a reply.
fn route_reply_line(line: &str, pending: &mut VecDeque<oneshot::Sender<Result<String>>>) {
    if let Some(url) = line.strip_prefix("URL ") {
        if let Some(reply) = pending.pop_front() {
            let _ = reply.send(Ok(url.trim().to_string()));
        } else {
            tracing::warn!("pluto: a URL line with no pending request; dropped");
        }
    } else if let Some(err) = line.strip_prefix("ERR ") {
        if let Some(reply) = pending.pop_front() {
            let _ = reply.send(Err(anyhow!("pluto: {err}")));
        } else {
            tracing::warn!(%line, "pluto ERR without pending request");
        }
    } else {
        tracing::debug!(target: "pluto.stdout", "{line}");
    }
}

#[cfg(test)]
#[path = "pluto_seams_tests.rs"]
pub(crate) mod seams;

async fn supervisor_task(
    mut contained: sot_containment::Contained,
    mut stdin: ChildStdin,
    mut stdout_lines: sot_protocol::child_lines::ChildLines<tokio::process::ChildStdout>,
    mut submit_rx: mpsc::Receiver<Submission>,
    ended: oneshot::Receiver<()>,
    sig: &'static sot_containment::Signal,
    grant: Option<Grant>,
    mut tmp: crate::sidecars::child::ChildTmp,
    stderr: StderrTail,
) {
    // FIFO of in-flight oneshots. Pluto's serial line protocol replies
    // to each OPEN in order; we pop the matching reply on each URL/ERR. A
    // submission joins it before its line is written, so every way out below
    // reaches it.
    let mut pending: VecDeque<oneshot::Sender<Result<String>>> = VecDeque::new();

    let cause = {
        // One observation of the child's exit, held across every await below, so a stdin write that cannot
        // complete (the child, or a descendant holding the pipe, reads nothing) still gives way to the exit.
        let exited = contained.wait_until_exited();
        tokio::pin!(exited);
        // Emptying the slot (an open past its bound, or the handle's drop) ends the child even mid-write.
        tokio::pin!(ended);
        loop {
            tokio::select! {
                biased;
                // The daemon is shutting down: the signal has already killed the
                // child's tree.
                _ = sig.fired() => break Cause::Signal,
                // The child's exit, not its pipes' EOF: a descendant that
                // inherited stdout keeps it open past the leader's death.
                r = &mut exited => break Cause::Exited(r),
                _ = tmp.display_ended() => break Cause::Display,
                _ = &mut ended => break Cause::Released,
                sub = submit_rx.recv() => {
                    let Some(sub) = sub else { break Cause::Closed };
                    pending.push_back(sub.reply);
                    let line = format!("OPEN {}\n", sub.abs_path);
                    let write = async {
                        stdin.write_all(line.as_bytes()).await?;
                        #[cfg(test)]
                        seams::before_flush().await?;
                        stdin.flush().await
                    };
                    #[cfg(test)]
                    let write = seams::observed(write);
                    let written = tokio::select! {
                        biased;
                        _ = sig.fired() => Err(Cause::Signal),
                        r = &mut exited => Err(Cause::Exited(r)),
                        _ = tmp.display_ended() => Err(Cause::Display),
                        _ = &mut ended => Err(Cause::Released),
                        r = write => r.map_err(Cause::Stdin),
                    };
                    // A write cut short may have sent part of the line: the
                    // supervisor ends and the line is never resent.
                    if let Err(cause) = written {
                        break cause;
                    }
                }
                line = stdout_lines.next_line() => {
                    match line {
                        Ok(Some(line)) => route_reply_line(&line, &mut pending),
                        Ok(None) => break Cause::Stdout("closed".to_string()),
                        Err(e) => break Cause::Stdout(e.to_string()),
                    }
                }
            }
        }
    };
    tracing::warn!(cause = %cause.describe(), "pluto supervisor ending");

    // What the child wrote before it exited still answers its requests.
    if matches!(cause, Cause::Exited(_)) {
        for _ in 0..64 {
            match tokio::time::timeout(std::time::Duration::from_millis(50), stdout_lines.next_line()).await {
                Ok(Ok(Some(line))) => route_reply_line(&line, &mut pending),
                _ => break,
            }
        }
    }

    // One closeout, before any cleanup await: release the proxy grant,
    // admit nothing more, and fail every request still waiting.
    drop(grant);
    submit_rx.close();
    let why = cause.describe();
    // `recv` after `close` also yields a request pushed after the drain began (the permit taken before the close).
    while let Some(sub) = submit_rx.recv().await {
        let _ = sub.reply.send(Err(anyhow!("pluto sidecar terminated: {why}")));
    }
    for reply in pending.drain(..) {
        let _ = reply.send(Err(anyhow!("pluto sidecar terminated: {why}")));
    }
    #[cfg(test)]
    {
        let gate = seams::CLEANUP_GATE.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(gate) = gate {
            let _ = gate.acquire().await;
        }
    }
    drop(stdin);
    if let Err(e) = tmp.retire(&mut contained).await {
        tracing::warn!(error = %e, "pluto child cleanup failed");
    }
    if !matches!(cause, Cause::Signal) {
        stderr.warn(&format!("pluto server ended: {why}")).await;
    }
}

#[cfg(test)]
#[path = "pluto_port_parse_tests.rs"]
mod port_parse_tests;

#[cfg(all(test, unix))]
#[path = "pluto_open_bound_tests.rs"]
mod open_bound_tests;
