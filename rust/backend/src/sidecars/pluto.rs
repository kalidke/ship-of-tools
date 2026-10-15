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
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::sidecars::child::StderrTail;

#[derive(Clone)]
pub struct Pluto {
    inner: Arc<PlutoInner>,
}

struct PlutoInner {
    project_dir: PathBuf,
    start_script: PathBuf,
    submit: Mutex<Option<Running>>,
}

/// The supervisor of one running server: submissions go in on `submit`, and dropping `_ends` ends the server.
struct Running {
    submit: mpsc::Sender<Submission>,
    _ends: oneshot::Sender<()>,
}

/// How long one open may take, from its submission to its `URL` or `ERR` line (Pluto loads the notebook before it
/// answers). An open past it is an error, and its server is ended: replies are routed oldest-first, so a late `URL`
/// would answer the next request.
const PLUTO_OPEN_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a start waits for the server's `READY` line before it ends the child. A first start after a Julia, package
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
        crate::paths::resource_dir("julia/pluto")
    }

    /// `<repo>/julia/pluto/start.jl` resolved via `CARGO_MANIFEST_DIR`.
    pub fn default_start_script() -> PathBuf {
        let mut p = Self::default_project_dir();
        p.push("start.jl");
        p
    }

    pub async fn open_notebook(&self, abs_path: &Path) -> Result<String> {
        let path_str = abs_path
            .to_str()
            .ok_or_else(|| anyhow!("pluto: path not utf-8: {}", abs_path.display()))?
            .to_string();
        let tx = self.ensure_supervisor().await?;
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
            return result;
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

    async fn ensure_supervisor(&self) -> Result<mpsc::Sender<Submission>> {
        let mut guard = self.inner.submit.lock().await;
        if let Some(running) = guard.as_ref() {
            if !running.submit.is_closed() {
                return Ok(running.submit.clone());
            }
        }
        let (julia_bin, _) = crate::sidecars::julia::resolve_bin().map_err(|e| anyhow!(e))?;
        let running = spawn_supervisor(
            &julia_bin,
            &self.inner.project_dir,
            &self.inner.start_script,
            crate::lifecycle::child_signal::process(),
        )
        .await?;
        let tx = running.submit.clone();
        *guard = Some(running);
        Ok(tx)
    }
}

async fn spawn_supervisor(
    julia_bin: &str,
    project_dir: &Path,
    start_script: &Path,
    sig: &'static crate::lifecycle::child_signal::Signal,
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
    // Counted and contained from spawn; the supervisor task takes it over once READY.
    let mut contained = sig
        .spawn(&mut cmd)
        .with_context(|| format!("spawn {julia_bin} --project={}", project_dir.display()))?;

    let stdin = contained.stdin.take().context("pluto child stdin missing")?;
    let stdout = contained.stdout.take().context("pluto child stdout missing")?;
    let stderr = contained.stderr.take().context("pluto child stderr missing")?;

    let stderr = StderrTail::drain(stderr, |line| tracing::debug!(target: "pluto.stderr", "{line}"));
    let mut stdout_lines = sot_protocol::child_lines::ChildLines::new(stdout);

    // Wait for the READY line before accepting any requests: the supervisor
    // task can't usefully service OPEN requests until the server is
    // actually listening, so block here once on first spawn.
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
                let _ = tmp.retire(&mut contained).await;
                return Err(anyhow!("the daemon is shutting down"));
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
    let base_url = match ready {
        Ok(base_url) => base_url,
        Err(e) => {
            let _ = tmp.retire(&mut contained).await;
            stderr.warn(&format!("{e:#}")).await;
            return Err(e);
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

    let (submit_tx, submit_rx) = mpsc::channel::<Submission>(64);
    let (ends, ended) = oneshot::channel();
    tokio::spawn(supervisor_task(contained, stdin, stdout_lines, submit_rx, ended, sig, grant, tmp, stderr));
    Ok(Running { submit: submit_tx, _ends: ends })
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
pub(crate) mod seams {
    //! Private test leaves at the real awaits of the supervisor: a count of the polls on which a stdin write was
    //! still pending, and a gate every closeout waits on before the checked termination.
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex};

    pub(crate) static WRITE_PENDING_POLLS: AtomicU64 = AtomicU64::new(0);
    /// Held (zero permits) while a test wants the flush await to stay pending; one-shot error for the flush.
    pub(crate) static FLUSH_GATE: Mutex<Option<Arc<tokio::sync::Semaphore>>> = Mutex::new(None);
    pub(crate) static FAIL_FLUSH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    pub(crate) static CLEANUP_GATE: Mutex<Option<Arc<tokio::sync::Semaphore>>> = Mutex::new(None);

    /// The leaf in front of the real flush: waits on the gate, then fails once if asked.
    pub(crate) async fn before_flush() -> std::io::Result<()> {
        let gate = FLUSH_GATE.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(gate) = gate {
            let _ = gate.acquire().await;
        }
        if FAIL_FLUSH.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::other("injected flush error"));
        }
        Ok(())
    }

    pub(crate) async fn observed<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        std::future::poll_fn(|cx| {
            let polled = future.as_mut().poll(cx);
            if polled.is_pending() {
                WRITE_PENDING_POLLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            polled
        })
        .await
    }
}

async fn supervisor_task(
    mut contained: crate::lifecycle::child_signal::Contained,
    mut stdin: ChildStdin,
    mut stdout_lines: sot_protocol::child_lines::ChildLines<tokio::process::ChildStdout>,
    mut submit_rx: mpsc::Receiver<Submission>,
    ended: oneshot::Receiver<()>,
    sig: &'static crate::lifecycle::child_signal::Signal,
    grant: Option<Grant>,
    tmp: crate::sidecars::child::ChildTmp,
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
    while let Ok(sub) = submit_rx.try_recv() {
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
mod port_parse_tests {
    use super::spawn_supervisor;
    use std::time::Duration;
    use {
        super::{bound_pluto_port, seams, Running, Submission},
        crate::lifecycle::child_signal::Signal,
        crate::sidecars::contract_tests::{executable, isolated, within},
        std::path::{Path, PathBuf},
        tokio::sync::oneshot,
    };

    /// The shutdown signal kills a Pluto child that has not yet said READY,
    /// and the test waits for its containment registration.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_kills_the_pluto_child_during_the_ready_wait() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub-julia");
        let ready = dir.path().join("started");
        sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\necho started > '{}'\nexec sleep 30\n", ready.display()));
        let script = dir.path().join("start.jl");
        std::fs::write(&script, "").unwrap();
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let task = tokio::spawn(async move { spawn_supervisor(&bin, &project, &script, sig).await.map(|_| ()) });
        let began = std::time::Instant::now();
        while std::fs::read_to_string(&ready).map_or(true, |text| text.trim() != "started") {
            assert!(began.elapsed() < Duration::from_secs(5), "the stub child never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        sig.fire().expect("fire");
        let result = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the READY wait outlived the shutdown")
            .expect("spawn task");
        assert!(result.is_err());
        assert!(sig.held_groups().is_empty());
    }

    /// A process the sidecar started dies with it: the stub starts a
    /// grandchild, says READY, and the shutdown must take both.
    #[cfg(unix)]
    #[tokio::test]
    async fn pluto_grandchild_dies_with_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let gc_file = dir.path().join("gc");
        let gc = crate::lifecycle::child_signal::tests::Leftover::of_file(gc_file.clone());
        let stub = dir.path().join("stub-julia");
        sot_log::test_exec::write_executable(
            &stub,
            format!("#!/bin/sh\nsleep 3103 &\necho $! > {}\necho \"READY http://127.0.0.1:1/\"\nexec sleep 3103\n", gc_file.display()),
        );
        let script = dir.path().join("start.jl");
        std::fs::write(&script, "").unwrap();
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let tx = spawn_supervisor(&bin, &project, &script, sig).await.expect("spawn_supervisor");
        let began = std::time::Instant::now();
        while std::fs::read_to_string(&gc_file).map(|s| s.trim().is_empty()).unwrap_or(true) {
            assert!(began.elapsed() < Duration::from_secs(5), "the stub never wrote its grandchild pid");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        sig.fire().expect("fire");
        let gone = gc.gone();
        drop(tx);
        assert!(gone, "the Pluto grandchild survived the shutdown");
    }

    /// A Pluto whose first boot outlasts the old 180 s bound (a cold precompile) is served, not ended. The paused clock
    /// passes 181 s while the stand-in waits, then the stand-in says READY; a running blocking task keeps tokio from
    /// moving the clock on its own (`tokio::time::pause`), so every wait here is the test's real-time poll.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_pluto_ready_after_the_old_180s_bound_is_served() {
        let dir = tempfile::tempdir().unwrap();
        let (started, go) = (dir.path().join("started"), dir.path().join("go"));
        let stub = dir.path().join("stub-julia");
        sot_log::test_exec::write_executable(
            &stub,
            format!(
                "#!/bin/sh\necho x > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.05; done\necho 'READY http://pluto.invalid/'\nexec sleep 30\n",
                started.display(),
                go.display()
            ),
        );
        let script = dir.path().join("start.jl");
        std::fs::write(&script, "").unwrap();
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        let (hold, held) = std::sync::mpsc::channel::<()>();
        let still = tokio::task::spawn_blocking(move || held.recv());
        let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let task = tokio::spawn(async move { spawn_supervisor(&bin, &project, &script, sig).await });
        async fn real_wait(what: &str, mut done: impl FnMut() -> bool) {
            let began = std::time::Instant::now();
            while !done() {
                assert!(began.elapsed() < Duration::from_secs(20), "not within 20 s: {what}");
                tokio::task::yield_now().await;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // The stand-in runs only after the spawn's poll has set the READY deadline.
        real_wait("the stand-in starts", || started.exists()).await;
        tokio::time::advance(Duration::from_secs(181)).await;
        std::fs::write(&go, "").unwrap();
        real_wait("the start ends", || task.is_finished()).await;
        drop(hold);
        let _ = still.await;
        let running = task.await.expect("the start task").unwrap_or_else(|e| panic!("a Pluto that says READY after 181 s was refused: {e:#}"));
        sig.fire().expect("fire");
        drop(running);
        real_wait("the stand-in is reaped", || sig.live() == 0).await;
    }

    /// A Pluto that fails its boot leaves its own words in the log at warn: the stand-in writes Julia's error to stderr
    /// and exits before READY.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_pluto_that_fails_its_boot_logs_its_stderr_at_warn() {
        let log = sot_log::test_log::capture();
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub-julia");
        sot_log::test_exec::write_executable(&stub, "#!/bin/sh\necho 'ERROR: LoadError: (pluto-boot-red)' >&2\nexit 1\n");
        let script = dir.path().join("start.jl");
        std::fs::write(&script, "").unwrap();
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let err = spawn_supervisor(&bin, &project, &script, sig).await.err().expect("a child that exits before READY");
        assert!(format!("{err:#}").contains("before READY"), "{err:#}");
        within(Duration::from_secs(5), "the child's stderr is in the log at warn", || crate::sidecars::warned(&log.text(), "pluto-boot-red")).await;
    }

    /// A Pluto line may hold any byte (an `ERR` line quotes what failed): the open it answers gets U+FFFD for the byte,
    /// and the same server answers the next open. The READY address is no loopback page, so no proxy grant is published.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_pluto_line_holding_an_invalid_byte_keeps_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub-julia");
        sot_log::test_exec::write_executable(
            &stub,
            "#!/bin/sh\necho 'READY http://pluto.invalid/'\nread l\nprintf 'ERR a\\377b\\n'\nread l\necho 'URL http://pluto.invalid/edit'\nexec sleep 600\n".to_string(),
        );
        let script = dir.path().join("start.jl");
        std::fs::write(&script, "").unwrap();
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let running = spawn_supervisor(&bin, &project, &script, sig).await.expect("spawn_supervisor");
        let mut answers = Vec::new();
        for path in ["/a.jl", "/b.jl"] {
            let (reply, rx) = oneshot::channel();
            let answer = match running.submit.send(Submission { abs_path: path.to_string(), reply }).await {
                Ok(()) => match tokio::time::timeout(Duration::from_secs(10), rx).await.expect("an answer within 10 s") {
                    Ok(answer) => answer.map_err(|e| format!("{e:#}")),
                    Err(_) => Err("the reply was dropped".to_string()),
                },
                Err(_) => Err("the server ended".to_string()),
            };
            answers.push(answer);
        }
        sig.fire().expect("fire");
        drop(running);
        assert_eq!(answers, [Err("pluto: a\u{fffd}b".to_string()), Ok("http://pluto.invalid/edit".to_string())]);
    }

    /// The longest an isolated body here may take.
    const BODY: Duration = Duration::from_secs(180);
    /// How long a fixture waits for the supervisor to react to the child's exit or the Signal.
    const REACT: Duration = Duration::from_secs(30);

    /// An owned `start.jl` standing in for Pluto's: it binds a real loopback listener, says READY, records its pid,
    /// optionally starts a descendant that keeps the child's pipes, and exits when the test creates `gate`.
    fn start_script(dir: &Path, ready_url: &str, descendant: &str) -> PathBuf {
        let script = dir.join("start.jl");
        let text = format!(
            r#"using Sockets
dir = raw"{dir}"
server = listen(ip"127.0.0.1", 0)
port = Int(getsockname(server)[2])
write(joinpath(dir, "port"), string(port))
write(joinpath(dir, "pid"), string(getpid()))
println({ready}); flush(stdout)
{descendant}
while !isfile(joinpath(dir, "gate")) sleep(0.05) end
write(joinpath(dir, "exiting"), "x")
exit(0)
"#,
            dir = dir.display(),
            ready = ready_url,
        );
        std::fs::write(&script, text).unwrap();
        script
    }

    const READY_LOCAL: &str = r#""READY http://127.0.0.1:$port""#;
    /// On Windows libuv puts every child it starts in a job that dies with its parent, so the descendant is detached
    /// there to outlive the child (it stays in the daemon's containment job); on Unix it stays in the child's group.
    /// A descendant that holds only the child's stdout.
    const KEEP_STDOUT: &str = r#"cmd = `$(Base.julia_cmd()) --startup-file=no -e "sleep(300)"`; p = run(pipeline(Sys.iswindows() ? detach(cmd) : cmd, stdout=stdout), wait=false); write(joinpath(dir, "desc"), string(getpid(p)))"#;
    /// A descendant that holds the read end of the child's stdin and the write end of its stdout, reading nothing.
    const KEEP_PIPES: &str = r#"cmd = `$(Base.julia_cmd()) --startup-file=no -e "sleep(300)"`; p = run(pipeline(Sys.iswindows() ? detach(cmd) : cmd, stdin=stdin, stdout=stdout), wait=false); write(joinpath(dir, "desc"), string(getpid(p)))"#;

    struct Harness {
        dir: PathBuf,
        sig: &'static Signal,
        tx: Running,
    }

    impl Harness {
        async fn start(ready_url: &str, descendant: &str) -> Harness {
            let root = tempfile::tempdir().expect("owned fixture root");
            let dir = root.path().to_path_buf();
            let script = start_script(&dir, ready_url, descendant);
            let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
            let julia = executable("julia").to_string_lossy().into_owned();
            let tx = spawn_supervisor(&julia, &dir, &script, sig).await.expect("the fixture says READY");
            let dir = root.keep();
            Harness { dir, sig, tx }
        }

        fn number(&self, name: &str) -> Option<u32> {
            std::fs::read_to_string(self.dir.join(name)).ok()?.trim().parse().ok()
        }

        async fn wait_number(&self, name: &str) -> u32 {
            within(REACT, name, || self.number(name).is_some()).await;
            self.number(name).unwrap()
        }

        async fn submit(&self, path: String) -> oneshot::Receiver<anyhow::Result<String>> {
            let (reply, rx) = oneshot::channel();
            self.tx.submit.send(Submission { abs_path: path, reply }).await.expect("the supervisor admits a submission");
            rx
        }

        /// Lets the child exit on its own.
        fn open_gate(&self) {
            std::fs::write(self.dir.join("gate"), "x").unwrap();
        }

        async fn finish(self) {
            self.sig.fire().expect("fire");
            within(REACT, "owned children reaped", || self.sig.live() == 0).await;
            std::fs::remove_dir_all(&self.dir).expect("remove the owned fixture root");
        }
    }

    /// Whether the fixture's child has begun exiting: its script writes `exiting` as its last act before `exit(0)`,
    /// so the observation is the same on every platform (the supervisor, not the test, waits for the child).
    fn exited(h: &Harness) -> bool {
        h.dir.join("exiting").exists()
    }

    /// Whether a process of this id is running (a zombie is not).
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only probes.
        let probe = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
        #[cfg(target_os = "linux")]
        if std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(')').map(|(_, rest)| rest.trim_start().starts_with('Z')))
            .unwrap_or(false)
        {
            return false;
        }
        probe
    }

    #[cfg(windows)]
    fn alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        // SAFETY: a plain query on a process handle opened and closed here.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == 259
        }
    }

    async fn errors(rx: oneshot::Receiver<anyhow::Result<String>>, what: &str) {
        let got = tokio::time::timeout(REACT, rx).await.unwrap_or_else(|_| panic!("{what}: no answer"));
        assert!(matches!(got, Ok(Err(_))), "{what}: must end with an error");
    }

    fn granted(port: u16) -> bool {
        bound_pluto_port() == Some(port) || crate::pages::proxy::allowed_proxy_ports().contains(&port)
    }

    /// The child's own exit, with no descendant, takes the proxy grant.
    #[tokio::test]
    async fn death_releases_proxy_grant() {
        if !isolated("sidecars::pluto::port_parse_tests::death_releases_proxy_grant", BODY) {
            return;
        }
        let h = Harness::start(READY_LOCAL, "").await;
        let port = h.wait_number("port").await as u16;
        assert!(granted(port), "setup: READY grants the port");
        h.open_gate();
        within(REACT, "the child exits", || exited(&h)).await;
        within(REACT, "the grant is released", || !granted(port)).await;
        h.finish().await;
    }

    /// A descendant that keeps stdout open past the child's exit does not keep the grant.
    #[tokio::test]
    async fn inherited_stdout_does_not_keep_grant() {
        if !isolated("sidecars::pluto::port_parse_tests::inherited_stdout_does_not_keep_grant", BODY) {
            return;
        }
        *seams::CLEANUP_GATE.lock().unwrap() = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(0)));
        let h = Harness::start(READY_LOCAL, KEEP_STDOUT).await;
        let port = h.wait_number("port").await as u16;
        let descendant = h.wait_number("desc").await;
        h.open_gate();
        within(REACT, "the child exits", || exited(&h)).await;
        within(REACT, "the grant is released", || !granted(port)).await;
        assert!(alive(descendant), "the release must not wait for cleanup or EOF");
        let gate = seams::CLEANUP_GATE.lock().unwrap().clone().unwrap();
        gate.add_permits(1);
        within(REACT, "cleanup ends the descendant", || !alive(descendant)).await;
        h.finish().await;
    }

    /// A READY line that is not a loopback page address grants nothing.
    #[tokio::test]
    async fn nonloopback_ready_grants_nothing() {
        if !isolated("sidecars::pluto::port_parse_tests::nonloopback_ready_grants_nothing", BODY) {
            return;
        }
        // A documentation-range address: never dialled.
        let h = Harness::start(r#""READY http://192.0.2.7:4000""#, "").await;
        assert_eq!(bound_pluto_port(), None);
        assert!(!crate::pages::proxy::allowed_proxy_ports().contains(&4000));
        h.finish().await;
    }

    /// A Pluto child the daemon kills leaves no temporary file: what it made in its temporary folder goes with the
    /// folder after the supervisor's reap.
    #[tokio::test]
    async fn a_killed_child_leaves_no_temp_files() {
        if !isolated("sidecars::pluto::port_parse_tests::a_killed_child_leaves_no_temp_files", BODY) {
            return;
        }
        // This process's temporary folder is an owned one, so a file the child leaves outside its own folder stays
        // here.
        let tmp = tempfile::tempdir().expect("owned temporary folder");
        for key in if cfg!(windows) { &["TMP", "TEMP"][..] } else { &["TMPDIR"][..] } {
            std::env::set_var(key, tmp.path());
        }
        let h = Harness::start(READY_LOCAL, r#"write(joinpath(dir, "made"), mktemp()[1])"#).await;
        within(REACT, "the child makes its temporary file", || h.dir.join("made").exists()).await;
        let made = PathBuf::from(std::fs::read_to_string(h.dir.join("made")).unwrap());
        assert!(made.exists(), "setup: the child's temporary file exists");
        h.sig.fire().expect("fire");
        within(REACT, "the killed child's temporary file is gone", || !made.exists()).await;
        h.finish().await;
    }

    /// Pluto's server has a display of its own (Linux), which its notebook workers inherit with the rest of its
    /// environment, and which ends with the server: the child's own exit here.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pluto_has_its_own_display_which_ends_with_it() {
        use crate::sidecars::contract_tests::{x11_answer, x11_cookie};
        if !isolated("sidecars::pluto::port_parse_tests::pluto_has_its_own_display_which_ends_with_it", BODY) {
            return;
        }
        executable("Xvfb");
        let record = r#"write(joinpath(dir, "display.tmp"), get(ENV, "DISPLAY", "-"), "\n", get(ENV, "XAUTHORITY", "-")); mv(joinpath(dir, "display.tmp"), joinpath(dir, "display"))"#;
        let h = Harness::start(READY_LOCAL, record).await;
        within(REACT, "the server records its display", || h.dir.join("display").exists()).await;
        let named = std::fs::read_to_string(h.dir.join("display")).unwrap();
        let (display, cookie) = named.split_once('\n').expect("two lines");
        let number: u32 = display.strip_prefix(':').and_then(|n| n.parse().ok()).unwrap_or_else(|| {
            panic!("the server names a display of its own: {named:?}")
        });
        let cookie = x11_cookie(Path::new(cookie));
        assert_eq!(x11_answer(number, Some(&cookie)), Some(1), "the server's cookie opens its display");
        h.open_gate();
        within(REACT, "the server's display ends with it", || x11_answer(number, Some(&cookie)) != Some(1)).await;
        h.finish().await;
    }

    /// A stand-in for Pluto's julia that names its display in the file `named` (before READY, when `ready`), starts a
    /// descendant in its group when `descendant`, and reads no request; with the Signal it is started under.
    #[cfg(target_os = "linux")]
    fn display_stand_in(dir: &Path, named: &Path, ready: bool, descendant: bool) -> (String, PathBuf, &'static Signal) {
        let stub = dir.join("stub-julia");
        sot_log::test_exec::write_executable(
            &stub,
            format!(
                "#!/bin/sh\n{child}printf '%s' \"$XAUTHORITY\" > '{named}.tmp'\nmv '{named}.tmp' '{named}'\n{ready}exec sleep 300\n",
                child = if descendant { "sleep 300 &\n" } else { "" },
                named = named.display(),
                ready = if ready { "echo 'READY http://pluto.invalid/'\n" } else { "" },
            ),
        );
        let script = dir.join("start.jl");
        std::fs::write(&script, "").unwrap();
        (stub.to_string_lossy().into_owned(), script, Box::leak(Box::new(Signal::new())))
    }

    /// A Pluto server whose display ends under it ends with what it started in its group, so nothing of it can open
    /// that display's name once another account binds it, and an open in flight says why.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_pluto_whose_display_ends_ends_with_it() {
        use crate::sidecars::contract_tests::{holding_display, x11_server};
        executable("Xvfb");
        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("named");
        let (bin, script, sig) = display_stand_in(dir.path(), &named, true, true);
        let running = spawn_supervisor(&bin, dir.path(), &script, sig).await.expect("the stand-in says READY");
        let cookie = PathBuf::from(std::fs::read_to_string(&named).expect("the stand-in names its display"));
        within(REACT, "setup: the server and its sleep hold the display", || holding_display(&cookie).len() == 2).await;
        let (reply, open) = oneshot::channel();
        running.submit.send(Submission { abs_path: "/nowhere.jl".into(), reply }).await.expect("the supervisor admits an open");
        // SAFETY: a signal to the X server started for this test's stand-in.
        assert_eq!(unsafe { libc::kill(x11_server(&cookie) as libc::pid_t, libc::SIGKILL) }, 0, "kill the display's server");
        within(REACT, "nothing of the server keeps its ended display", || holding_display(&cookie).is_empty()).await;
        let answer = tokio::time::timeout(REACT, open).await.expect("the open is answered").expect("the supervisor answers");
        let why = format!("{:#}", answer.expect_err("an open the display's end cut short fails"));
        assert!(why.contains("its display ended"), "{why}");
        within(REACT, "the server and its display are reaped", || sig.live() == 0).await;
        drop(running);
    }

    /// A display that ends while its server starts ends the start: the server is ended, and the start fails saying why.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_pluto_whose_display_ends_before_ready_is_not_served() {
        use crate::sidecars::contract_tests::{holding_display, x11_server};
        executable("Xvfb");
        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("named");
        let (bin, script, sig) = display_stand_in(dir.path(), &named, false, false);
        let project = dir.path().to_path_buf();
        let start = tokio::spawn(async move { spawn_supervisor(&bin, &project, &script, sig).await.map(|_| ()) });
        within(REACT, "the stand-in names its display", || named.exists()).await;
        let cookie = PathBuf::from(std::fs::read_to_string(&named).unwrap());
        // SAFETY: a signal to the X server started for this test's stand-in.
        assert_eq!(unsafe { libc::kill(x11_server(&cookie) as libc::pid_t, libc::SIGKILL) }, 0, "kill the display's server");
        let started = tokio::time::timeout(REACT, start).await.expect("the start ends with its display").expect("the start task");
        let why = format!("{:#}", started.expect_err("a server whose display ended is not served"));
        assert!(why.contains("display ended before READY"), "{why}");
        assert!(holding_display(&cookie).is_empty(), "nothing of the server keeps its ended display");
        assert_eq!(sig.live(), 0, "the server and its display are reaped");
    }

    /// A stdin write that cannot complete: three requests are in flight (one answered by nobody, one mid-write, one
    /// queued) when the child exits with a descendant holding both pipes.
    async fn blocked_write_scenario(h: &Harness) -> (u16, u32, Vec<oneshot::Receiver<anyhow::Result<String>>>) {
        let port = h.wait_number("port").await as u16;
        let descendant = h.wait_number("desc").await;
        let first = h.submit("a".to_string()).await;
        let before = seams::WRITE_PENDING_POLLS.load(std::sync::atomic::Ordering::SeqCst);
        let big = h.submit("x".repeat(4 * 1024 * 1024)).await;
        within(REACT, "the large write is observed blocked", || {
            seams::WRITE_PENDING_POLLS.load(std::sync::atomic::Ordering::SeqCst) > before
        })
        .await;
        let queued = h.submit("c".to_string()).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(alive(h.number("pid").unwrap()) && alive(descendant), "setup: child and descendant are running");
        assert!(granted(port), "setup: the grant is held");
        (port, descendant, vec![first, big, queued])
    }

    #[tokio::test]
    async fn blocked_write_death_releases_grant_and_closes_requests() {
        if !isolated("sidecars::pluto::port_parse_tests::blocked_write_death_releases_grant_and_closes_requests", BODY) {
            return;
        }
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        *seams::CLEANUP_GATE.lock().unwrap() = Some(gate.clone());
        let h = Harness::start(READY_LOCAL, KEEP_PIPES).await;
        let (port, descendant, replies) = blocked_write_scenario(&h).await;
        h.open_gate();
        within(REACT, "the child exits", || exited(&h)).await;
        within(REACT, "the grant is released", || !granted(port)).await;
        assert!(alive(descendant), "revocation precedes cleanup: the descendant still holds the pipes");
        for (i, rx) in replies.into_iter().enumerate() {
            errors(rx, &format!("request {i}")).await;
        }
        assert!(alive(descendant), "closeout precedes cleanup");
        gate.add_permits(1);
        within(REACT, "cleanup ends the descendant", || !alive(descendant)).await;
        h.finish().await;
    }

    #[tokio::test]
    async fn signal_cancels_blocked_pluto_write() {
        if !isolated("sidecars::pluto::port_parse_tests::signal_cancels_blocked_pluto_write", BODY) {
            return;
        }
        let h = Harness::start(READY_LOCAL, KEEP_PIPES).await;
        let (port, _descendant, replies) = blocked_write_scenario(&h).await;
        h.sig.fire().expect("fire");
        within(REACT, "the grant is released", || !granted(port)).await;
        for (i, rx) in replies.into_iter().enumerate() {
            errors(rx, &format!("request {i}")).await;
        }
        h.finish().await;
    }

    /// A flush that cannot finish gives way to the child's exit like a write does: the grant is released and the
    /// current and pending requests end with errors.
    #[tokio::test]
    async fn flush_blocked_death_releases_grant_and_closes_requests() {
        if !isolated("sidecars::pluto::port_parse_tests::flush_blocked_death_releases_grant_and_closes_requests", BODY) {
            return;
        }
        let h = Harness::start(READY_LOCAL, "").await;
        let port = h.wait_number("port").await as u16;
        let first = h.submit("a".to_string()).await;
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        *seams::FLUSH_GATE.lock().unwrap() = Some(gate.clone());
        let second = h.submit("b".to_string()).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(granted(port), "setup: the grant is held while the flush waits");
        h.open_gate();
        within(REACT, "the child exits", || exited(&h)).await;
        within(REACT, "the grant is released", || !granted(port)).await;
        errors(first, "first").await;
        errors(second, "second").await;
        h.finish().await;
    }

    /// A flush that fails ends the supervisor the same way: grant released, requests failed.
    #[tokio::test]
    async fn flush_error_releases_grant_and_closes_requests() {
        if !isolated("sidecars::pluto::port_parse_tests::flush_error_releases_grant_and_closes_requests", BODY) {
            return;
        }
        let h = Harness::start(READY_LOCAL, "").await;
        let port = h.wait_number("port").await as u16;
        seams::FAIL_FLUSH.store(true, std::sync::atomic::Ordering::SeqCst);
        let only = h.submit("a".to_string()).await;
        errors(only, "the request whose flush failed").await;
        within(REACT, "the grant is released", || !granted(port)).await;
        h.finish().await;
    }
}

#[cfg(all(test, unix))]
mod open_bound_tests {
    use super::Pluto;
    use crate::sidecars::contract_tests::{isolated, pin_env, within};
    use std::time::Duration;

    /// An open the server never answers is an error at `PLUTO_OPEN_BOUND`, and its server is ended: a second open
    /// starts a second one. The stand-in for julia says READY, then reads its lines and answers none.
    #[tokio::test]
    async fn an_open_the_child_never_answers_fails_at_its_bound() {
        if !isolated("sidecars::pluto::open_bound_tests::an_open_the_child_never_answers_fails_at_its_bound", Duration::from_secs(150)) {
            return;
        }
        let dir = tempfile::Builder::new().prefix("sot-pluto-bound-").tempdir().unwrap();
        let stub = dir.path().join("stub-julia");
        let starts = dir.path().join("starts");
        sot_log::test_exec::write_executable(
            &stub,
            format!(
                "#!/bin/sh\necho $$ >> '{}'\necho \"READY http://127.0.0.1:1/\"\nwhile read l; do :; done\n",
                starts.display()
            ),
        );
        let script = dir.path().join("start.jl");
        std::fs::write(&script, "").unwrap();
        let _pin = pin_env(&stub, dir.path());
        let pluto = Pluto::new(dir.path().to_path_buf(), script);
        let started = || std::fs::read_to_string(&starts).map(|s| s.lines().map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();

        let t0 = std::time::Instant::now();
        let err = tokio::time::timeout(Duration::from_secs(45), pluto.open_notebook(&dir.path().join("a.jl")))
            .await
            .expect("the open is still waiting at 45 s")
            .expect_err("an open the child never answers");
        let took = t0.elapsed();
        let bound = Duration::from_secs(30); // PLUTO_OPEN_BOUND, written out so this test also runs on the unbounded code
        assert!(took >= bound && took < bound + Duration::from_secs(5), "ended after {took:?}");
        let text = format!("{err:#}");
        assert!(text.contains("did not answer the open") && text.contains("within 30s"), "{text}");
        let first = started();
        assert_eq!(first.len(), 1);
        let pid: libc::pid_t = first[0].parse().unwrap();
        // SAFETY: signal 0 only probes.
        within(Duration::from_secs(5), "the unanswering server is ended", || unsafe { libc::kill(pid, 0) != 0 }).await;

        let next = {
            let pluto = pluto.clone();
            let path = dir.path().join("b.jl");
            tokio::spawn(async move { pluto.open_notebook(&path).await })
        };
        within(Duration::from_secs(5), "the next open starts a second server", || started().len() == 2).await;
        println!("pluto-bound: {text} after {took:?}; a second open started server {}", started().len());
        next.abort();
        drop(pluto);
    }
}
