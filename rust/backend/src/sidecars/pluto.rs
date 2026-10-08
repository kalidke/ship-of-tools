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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

#[derive(Clone)]
pub struct Pluto {
    inner: Arc<PlutoInner>,
}

struct PlutoInner {
    project_dir: PathBuf,
    start_script: PathBuf,
    submit: Mutex<Option<mpsc::Sender<Submission>>>,
}

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
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Submission {
            abs_path: path_str,
            reply: reply_tx,
        })
        .await
        .map_err(|_| anyhow!("pluto supervisor channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("pluto supervisor dropped reply channel"))?
    }

    async fn ensure_supervisor(&self) -> Result<mpsc::Sender<Submission>> {
        let mut guard = self.inner.submit.lock().await;
        if let Some(tx) = guard.as_ref() {
            if !tx.is_closed() {
                return Ok(tx.clone());
            }
        }
        let (julia_bin, _) = crate::sidecars::julia::resolve_bin().map_err(|e| anyhow!(e))?;
        let tx = spawn_supervisor(
            &julia_bin,
            &self.inner.project_dir,
            &self.inner.start_script,
            crate::lifecycle::child_signal::process(),
        )
        .await?;
        *guard = Some(tx.clone());
        Ok(tx)
    }
}

async fn spawn_supervisor(
    julia_bin: &str,
    project_dir: &Path,
    start_script: &Path,
    sig: &'static crate::lifecycle::child_signal::Signal,
) -> Result<mpsc::Sender<Submission>> {
    if !start_script.exists() {
        return Err(anyhow!(
            "pluto start script missing at {}",
            start_script.display()
        ));
    }
    let mut cmd = Command::new(julia_bin);
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

    // Stderr drain — pure logging.
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::debug!(target: "pluto.stderr", "{line}");
        }
    });

    let mut stdout_lines = BufReader::new(stdout).lines();

    // Wait for the READY line before accepting any requests. Julia
    // startup + Pluto load takes ~30s on a cold depot; the supervisor
    // task can't usefully service OPEN requests until the server is
    // actually listening, so block here once on first spawn.
    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
    let base_url: String = loop {
        let now = tokio::time::Instant::now();
        if now >= ready_deadline {
            let _ = contained.kill().await;
            return Err(anyhow!("pluto sidecar did not emit READY within 180s"));
        }
        let remaining = ready_deadline - now;
        let line = tokio::select! {
            line = tokio::time::timeout(remaining, stdout_lines.next_line()) => line,
            // The daemon is shutting down: the signal has already killed the
            // child's tree.
            _ = sig.fired() => {
                return Err(anyhow!("the daemon is shutting down"));
            }
        };
        match line {
            Ok(Ok(Some(line))) => {
                if let Some(rest) = line.strip_prefix("READY ") {
                    break rest.trim().to_string();
                } else {
                    tracing::warn!(line = %line, "pluto sidecar pre-READY chatter");
                }
            }
            Ok(Ok(None)) => {
                let _ = contained.kill().await;
                return Err(anyhow!("pluto sidecar stdout closed before READY"));
            }
            Ok(Err(e)) => {
                let _ = contained.kill().await;
                return Err(anyhow!("pluto sidecar stdout error: {e}"));
            }
            Err(_) => {
                let _ = contained.kill().await;
                return Err(anyhow!("pluto sidecar did not emit READY within 180s"));
            }
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
    tokio::spawn(supervisor_task(contained, stdin, stdout_lines, submit_rx, sig, grant));
    Ok(submit_tx)
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
    pub(crate) static CLEANUP_GATE: Mutex<Option<Arc<tokio::sync::Semaphore>>> = Mutex::new(None);

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
    mut stdout_lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    mut submit_rx: mpsc::Receiver<Submission>,
    sig: &'static crate::lifecycle::child_signal::Signal,
    grant: Option<Grant>,
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
        loop {
            tokio::select! {
                biased;
                // The daemon is shutting down: the signal has already killed the
                // child's tree.
                _ = sig.fired() => break Cause::Signal,
                // The child's exit, not its pipes' EOF: a descendant that
                // inherited stdout keeps it open past the leader's death.
                r = &mut exited => break Cause::Exited(r),
                sub = submit_rx.recv() => {
                    let Some(sub) = sub else { break Cause::Closed };
                    pending.push_back(sub.reply);
                    let line = format!("OPEN {}\n", sub.abs_path);
                    let write = async {
                        stdin.write_all(line.as_bytes()).await?;
                        stdin.flush().await
                    };
                    #[cfg(test)]
                    let write = seams::observed(write);
                    let written = tokio::select! {
                        biased;
                        _ = sig.fired() => Err(Cause::Signal),
                        r = &mut exited => Err(Cause::Exited(r)),
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
    if let Err(e) = contained.kill().await {
        tracing::warn!(error = %e, "pluto child cleanup failed");
    }
}

#[cfg(test)]
mod port_parse_tests {
    use super::spawn_supervisor;
    use std::time::Duration;
    #[cfg(target_os = "linux")]
    use {
        super::{bound_pluto_port, seams, Submission},
        crate::lifecycle::child_signal::Signal,
        crate::sidecars::contract_tests::{executable, isolated, within},
        std::path::{Path, PathBuf},
        tokio::sync::{mpsc, oneshot},
    };

    /// The shutdown signal kills a Pluto child that has not yet said READY,
    /// and the child is counted from spawn.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_kills_the_pluto_child_during_the_ready_wait() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub-julia");
        sot_log::test_exec::write_executable(&stub, "#!/bin/sh\nexec sleep 30\n");
        let script = dir.path().join("start.jl");
        std::fs::write(&script, "").unwrap();
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let task = tokio::spawn(async move { spawn_supervisor(&bin, &project, &script, sig).await.map(|_| ()) });
        let began = std::time::Instant::now();
        while sig.live() == 0 {
            assert!(began.elapsed() < Duration::from_secs(5), "the stub child never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        sig.fire();
        let result = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the READY wait outlived the shutdown")
            .expect("spawn task");
        assert!(result.is_err());
        assert_eq!(sig.live(), 0);
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
        sig.fire();
        let gone = gc.gone();
        drop(tx);
        assert!(gone, "the Pluto grandchild survived the shutdown");
    }

    #[cfg(target_os = "linux")]
    /// The longest an isolated body here may take.
    const BODY: Duration = Duration::from_secs(180);
    #[cfg(target_os = "linux")]
    /// How long a fixture waits for the supervisor to react to the child's exit or the Signal.
    const REACT: Duration = Duration::from_secs(30);

    #[cfg(target_os = "linux")]
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
exit(0)
"#,
            dir = dir.display(),
            ready = ready_url,
        );
        std::fs::write(&script, text).unwrap();
        script
    }

    #[cfg(target_os = "linux")]
    const READY_LOCAL: &str = r#""READY http://127.0.0.1:$port""#;
    #[cfg(target_os = "linux")]
    /// A descendant that holds only the child's stdout.
    const KEEP_STDOUT: &str = r#"p = run(pipeline(`sleep 300`, stdout=stdout), wait=false); write(joinpath(dir, "desc"), string(getpid(p)))"#;
    #[cfg(target_os = "linux")]
    /// A descendant that holds the read end of the child's stdin and the write end of its stdout, reading nothing.
    const KEEP_PIPES: &str = r#"p = run(pipeline(`sleep 300`, stdin=stdin, stdout=stdout), wait=false); write(joinpath(dir, "desc"), string(getpid(p)))"#;

    #[cfg(target_os = "linux")]
    struct Harness {
        dir: PathBuf,
        sig: &'static Signal,
        tx: mpsc::Sender<Submission>,
    }

    #[cfg(target_os = "linux")]
    impl Harness {
        async fn start(ready_url: &str, descendant: &str) -> Harness {
            let dir = tempfile::tempdir().expect("owned fixture root").keep();
            let script = start_script(&dir, ready_url, descendant);
            let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
            let julia = executable("julia").to_string_lossy().into_owned();
            let tx = spawn_supervisor(&julia, &dir, &script, sig).await.expect("the fixture says READY");
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
            self.tx.send(Submission { abs_path: path, reply }).await.expect("the supervisor admits a submission");
            rx
        }

        /// Lets the child exit on its own.
        fn open_gate(&self) {
            std::fs::write(self.dir.join("gate"), "x").unwrap();
        }

        async fn finish(self) {
            self.sig.fire();
            within(REACT, "owned children reaped", || self.sig.live() == 0).await;
            std::fs::remove_dir_all(&self.dir).expect("remove the owned fixture root");
        }
    }

    #[cfg(target_os = "linux")]
    /// Whether the process has exited and not been reaped (Linux), or is gone.
    fn exited(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat.rsplit_once(')').map(|(_, rest)| rest.trim_start().starts_with('Z')).unwrap_or(false),
            Err(_) => true,
        }
    }

    #[cfg(target_os = "linux")]
    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only probes.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 && !exited(pid) }
    }

    #[cfg(target_os = "linux")]
    async fn errors(rx: oneshot::Receiver<anyhow::Result<String>>, what: &str) {
        let got = tokio::time::timeout(REACT, rx).await.unwrap_or_else(|_| panic!("{what}: no answer"));
        assert!(matches!(got, Ok(Err(_))), "{what}: must end with an error");
    }

    #[cfg(target_os = "linux")]
    fn granted(port: u16) -> bool {
        bound_pluto_port() == Some(port) || crate::pages::proxy::allowed_proxy_ports().contains(&port)
    }

    /// The child's own exit, with no descendant, takes the proxy grant.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn death_releases_proxy_grant() {
        if !isolated("sidecars::pluto::port_parse_tests::death_releases_proxy_grant", BODY) {
            return;
        }
        let h = Harness::start(READY_LOCAL, "").await;
        let port = h.wait_number("port").await as u16;
        assert!(granted(port), "setup: READY grants the port");
        let child = h.wait_number("pid").await;
        h.open_gate();
        within(REACT, "the child exits", || exited(child)).await;
        within(REACT, "the grant is released", || !granted(port)).await;
        h.finish().await;
    }

    /// A descendant that keeps stdout open past the child's exit does not keep the grant.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn inherited_stdout_does_not_keep_grant() {
        if !isolated("sidecars::pluto::port_parse_tests::inherited_stdout_does_not_keep_grant", BODY) {
            return;
        }
        *seams::CLEANUP_GATE.lock().unwrap() = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(0)));
        let h = Harness::start(READY_LOCAL, KEEP_STDOUT).await;
        let port = h.wait_number("port").await as u16;
        let child = h.wait_number("pid").await;
        let descendant = h.wait_number("desc").await;
        h.open_gate();
        within(REACT, "the child exits", || exited(child)).await;
        within(REACT, "the grant is released", || !granted(port)).await;
        assert!(alive(descendant), "the release must not wait for cleanup or EOF");
        let gate = seams::CLEANUP_GATE.lock().unwrap().clone().unwrap();
        gate.add_permits(1);
        within(REACT, "cleanup ends the descendant", || !alive(descendant)).await;
        h.finish().await;
    }

    /// A READY line that is not a loopback page address grants nothing.
    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
    /// A stdin write that cannot complete: three requests are in flight (one answered by nobody, one mid-write, one
    /// queued) when the child exits with a descendant holding both pipes.
    async fn blocked_write_scenario(h: &Harness) -> (u16, u32, u32, Vec<oneshot::Receiver<anyhow::Result<String>>>) {
        let port = h.wait_number("port").await as u16;
        let child = h.wait_number("pid").await;
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
        assert!(alive(child) && alive(descendant), "setup: child and descendant are running");
        assert!(granted(port), "setup: the grant is held");
        (port, child, descendant, vec![first, big, queued])
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn blocked_write_death_releases_grant_and_closes_requests() {
        if !isolated("sidecars::pluto::port_parse_tests::blocked_write_death_releases_grant_and_closes_requests", BODY) {
            return;
        }
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        *seams::CLEANUP_GATE.lock().unwrap() = Some(gate.clone());
        let h = Harness::start(READY_LOCAL, KEEP_PIPES).await;
        let (port, child, descendant, replies) = blocked_write_scenario(&h).await;
        h.open_gate();
        within(REACT, "the child exits", || exited(child)).await;
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn signal_cancels_blocked_pluto_write() {
        if !isolated("sidecars::pluto::port_parse_tests::signal_cancels_blocked_pluto_write", BODY) {
            return;
        }
        let h = Harness::start(READY_LOCAL, KEEP_PIPES).await;
        let (port, _child, _descendant, replies) = blocked_write_scenario(&h).await;
        h.sig.fire();
        within(REACT, "the grant is released", || !granted(port)).await;
        for (i, rx) in replies.into_iter().enumerate() {
            errors(rx, &format!("request {i}")).await;
        }
        h.finish().await;
    }
}
