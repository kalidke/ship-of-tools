// pluto.rs — supervisor for the Pluto-notebook sidecar.
//
// One shared Pluto server per backend, lazy-spawned on the first
// `pluto.open` call and re-used across calls. The Julia child runs
// `julia --project=<repo>/julia/pluto <repo>/julia/pluto/start.jl`;
// the script starts a `Pluto.ServerSession` on 127.0.0.1:1234 (access-secret
// required, no auto-browser — security review: this is otherwise a second
// unauthenticated RCE on a shared host), prints `READY <base_url>` once the
// HTTP listener is bound, then services `OPEN <abspath>` requests on
// stdin, replying with `URL <url>` or `ERR <msg>` per line on stdout. The
// `URL` already carries `?secret=...` — Julia holds `session.secret` and
// builds the full URL itself, so this supervisor never needs to see it.
//
// Same lazy-respawn shape as mathjax.rs: on child death the
// supervisor drops the submission channel; the next caller respawns.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

#[derive(Clone)]
pub struct Pluto {
    inner: Arc<PlutoInner>,
}

struct PlutoInner {
    project_dir: PathBuf,
    start_script: PathBuf,
    julia_bin: String,
    submit: Mutex<Option<mpsc::Sender<Submission>>>,
}

struct Submission {
    abs_path: String,
    reply: oneshot::Sender<Result<String>>,
}

impl Pluto {
    pub fn new(project_dir: PathBuf, start_script: PathBuf) -> Self {
        let julia_bin = std::env::var("SOT_JULIA_BIN").unwrap_or_else(|_| "julia".to_string());
        Self {
            inner: Arc::new(PlutoInner {
                project_dir,
                start_script,
                julia_bin,
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
        let tx = spawn_supervisor(
            &self.inner.julia_bin,
            &self.inner.project_dir,
            &self.inner.start_script,
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
) -> Result<mpsc::Sender<Submission>> {
    if !start_script.exists() {
        return Err(anyhow!(
            "pluto start script missing at {}",
            start_script.display()
        ));
    }
    let mut child: Child = Command::new(julia_bin)
        .arg(format!("--project={}", project_dir.display()))
        .arg(start_script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {julia_bin} --project={}", project_dir.display()))?;

    let stdin = child.stdin.take().context("pluto child stdin missing")?;
    let stdout = child.stdout.take().context("pluto child stdout missing")?;
    let stderr = child.stderr.take().context("pluto child stderr missing")?;

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
            let _ = child.kill().await;
            return Err(anyhow!("pluto sidecar did not emit READY within 180s"));
        }
        let remaining = ready_deadline - now;
        match tokio::time::timeout(remaining, stdout_lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                if let Some(rest) = line.strip_prefix("READY ") {
                    break rest.trim().to_string();
                } else {
                    tracing::warn!(line = %line, "pluto sidecar pre-READY chatter");
                }
            }
            Ok(Ok(None)) => {
                let _ = child.kill().await;
                return Err(anyhow!("pluto sidecar stdout closed before READY"));
            }
            Ok(Err(e)) => {
                let _ = child.kill().await;
                return Err(anyhow!("pluto sidecar stdout error: {e}"));
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(anyhow!("pluto sidecar did not emit READY within 180s"));
            }
        }
    };
    tracing::info!(base_url = %base_url, "pluto sidecar ready");
    // Record the port the sidecar ACTUALLY bound (start.jl falls back to an
    // ephemeral port when 1234 is taken — another user's Pluto on a shared
    // host). The ADR-0035 proxy allowlist reads this so it authorizes the
    // real server, never a stranger's process squatting the preferred port.
    if let Some(port) = port_from_base_url(&base_url) {
        BOUND_PORT.store(port, std::sync::atomic::Ordering::SeqCst);
    } else {
        tracing::warn!(base_url = %base_url, "could not parse port from pluto READY url — proxy allowlist won't include pluto");
    }

    let (submit_tx, submit_rx) = mpsc::channel::<Submission>(64);
    tokio::spawn(supervisor_task(child, stdin, stdout_lines, submit_rx));
    Ok(submit_tx)
}

/// The port the Pluto sidecar ACTUALLY bound, parsed from its READY line
/// (0 = not started / parse failed). The proxy allowlist reads this instead
/// of assuming the preferred 1234.
static BOUND_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

pub fn bound_pluto_port() -> Option<u16> {
    match BOUND_PORT.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        p => Some(p),
    }
}

/// Parse the port out of a `http://127.0.0.1:<port>` base URL.
fn port_from_base_url(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    authority.rsplit_once(':')?.1.parse().ok()
}

async fn supervisor_task(
    mut child: Child,
    mut stdin: ChildStdin,
    mut stdout_lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    mut submit_rx: mpsc::Receiver<Submission>,
) {
    // FIFO of in-flight oneshots. Pluto's serial line protocol replies
    // to each OPEN in order; we pop the matching reply on each URL/ERR.
    let mut pending: VecDeque<oneshot::Sender<Result<String>>> = VecDeque::new();

    loop {
        tokio::select! {
            biased;
            sub = submit_rx.recv() => {
                let Some(sub) = sub else {
                    drop(stdin);
                    let _ = child.wait().await;
                    return;
                };
                let line = format!("OPEN {}\n", sub.abs_path);
                if let Err(e) = stdin.write_all(line.as_bytes()).await {
                    let _ = sub.reply.send(Err(anyhow!("pluto stdin: {e}")));
                    break;
                }
                if let Err(e) = stdin.flush().await {
                    let _ = sub.reply.send(Err(anyhow!("pluto stdin flush: {e}")));
                    break;
                }
                pending.push_back(sub.reply);
            }
            line = stdout_lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Some(url) = line.strip_prefix("URL ") {
                            if let Some(reply) = pending.pop_front() {
                                let _ = reply.send(Ok(url.trim().to_string()));
                            } else {
                                tracing::warn!(%line, "pluto URL without pending request");
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
                    Ok(None) => {
                        tracing::warn!("pluto child stdout closed");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "pluto stdout error");
                        break;
                    }
                }
            }
        }
    }

    for reply in pending.drain(..) {
        let _ = reply.send(Err(anyhow!("pluto sidecar terminated")));
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(test)]
mod port_parse_tests {
    use super::port_from_base_url;

    #[test]
    fn parses_ready_url_port() {
        assert_eq!(port_from_base_url("http://127.0.0.1:1234"), Some(1234));
        assert_eq!(port_from_base_url("http://127.0.0.1:43127/"), Some(43127));
        assert_eq!(port_from_base_url("http://127.0.0.1"), None);
        assert_eq!(port_from_base_url("garbage"), None);
    }
}
