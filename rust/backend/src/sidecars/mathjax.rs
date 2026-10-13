// mathjax.rs — supervisor for the MathJax-SVG sidecar.
//
// Per ADR 0012 the Ship of Tools frontend renders inline LaTeX as MathJax-SVG.
// The actual rendering happens in a long-lived `node` process running
// `rust/backend/sidecars/mathjax/render.mjs`; this module owns that
// child, multiplexes Rust callers onto its single stdin/stdout, routes
// responses back by request id.
//
// API: Callers
// `.render(latex, display).await` for SVG bytes, within `RENDER_BOUND`. On
// child death the supervisor drains in-flight oneshots with an error, then
// lazily relaunches on the next request; a render past its bound ends its
// child the same way.
//
// Lazy start: the child isn't spawned until the first render request, so
// backends launched without ever serving math don't pay the node-process
// startup cost.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

#[derive(Clone)]
pub struct MathJax {
    inner: Arc<MathJaxInner>,
}

struct MathJaxInner {
    /// Where the node script lives. Resolved at construction so we don't
    /// re-walk the filesystem on every restart.
    script_path: PathBuf,
    /// Path to the node binary; defaults to "node" on PATH but can be
    /// overridden via `SOT_NODE_BIN` for sandboxed deployments.
    node_bin: String,
    /// The running child, if any. When the supervisor exits, its channel
    /// breaks and we relaunch lazily on the next call; emptying the slot ends
    /// the child.
    submit: Mutex<Option<Running>>,
    /// The shutdown signal the child is started under and the service loop watches; supplied by the owner of
    /// this handle, never chosen inside supervision.
    sig: &'static crate::lifecycle::child_signal::Signal,
}

/// The supervisor of one running child: submissions go in on `submit`, and dropping `_ends` ends the child.
struct Running {
    submit: mpsc::Sender<Submission>,
    _ends: oneshot::Sender<()>,
}

/// How long one render may take, from its submission to its SVG: the kernel's `KERNEL_REQUEST_TIMEOUT` twin. A render
/// past it is an error, and its child is ended so the next render starts a fresh one.
const RENDER_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

struct Submission {
    latex: String,
    display: bool,
    reply: oneshot::Sender<Result<RenderedSvg>>,
}

#[derive(Debug, Clone)]
pub struct RenderedSvg {
    pub svg: Vec<u8>,
    pub ex: f32,
}

#[derive(Deserialize)]
struct WireResponse {
    id: u64,
    #[serde(default)]
    svg: Option<String>,
    #[serde(default)]
    ex: Option<f32>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    id: u64,
    tex: &'a str,
    display: bool,
}

impl MathJax {
    pub fn new(script_path: PathBuf, sig: &'static crate::lifecycle::child_signal::Signal) -> Self {
        let node_bin = std::env::var("SOT_NODE_BIN").unwrap_or_else(|_| "node".to_string());
        Self {
            inner: Arc::new(MathJaxInner {
                script_path,
                node_bin,
                submit: Mutex::new(None),
                sig,
            }),
        }
    }

    /// Default location for the rendering script — repo-root-relative
    /// `rust/backend/sidecars/mathjax/render.mjs`, resolved for both layouts
    /// (dev checkout / release install — the julia bundle packs the sidecar
    /// at that same repo-shaped path) via `paths::resource_dir` (ADR 0030 §4).
    /// Can be overridden at runtime by passing a different path to `new`.
    pub fn default_script_path() -> PathBuf {
        crate::paths::resource_dir("rust/backend/sidecars/mathjax/render.mjs")
    }

    /// Convert LaTeX to SVG via the sidecar, within `RENDER_BOUND`. Spawns the
    /// child on first call, reuses across subsequent calls, relaunches on
    /// death. A render past its bound ends that child.
    pub async fn render(&self, latex: &str, display: bool) -> Result<RenderedSvg> {
        let tx = self.ensure_supervisor().await?;
        let rendered = tokio::time::timeout(RENDER_BOUND, async {
            let (reply_tx, reply_rx) = oneshot::channel();
            tx.send(Submission {
                latex: latex.to_string(),
                display,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow!("mathjax supervisor channel closed"))?;
            reply_rx
                .await
                .map_err(|_| anyhow!("mathjax supervisor dropped reply channel"))?
        })
        .await;
        if let Ok(result) = rendered {
            return result;
        }
        // A child that does not answer within the bound is ended, unless a
        // later render has already replaced it.
        let mut guard = self.inner.submit.lock().await;
        if guard.as_ref().is_some_and(|running| running.submit.same_channel(&tx)) {
            *guard = None;
        }
        Err(anyhow!("mathjax render timed out after {RENDER_BOUND:?}; its child was ended"))
    }

    async fn ensure_supervisor(&self) -> Result<mpsc::Sender<Submission>> {
        let mut guard = self.inner.submit.lock().await;
        if let Some(running) = guard.as_ref() {
            if !running.submit.is_closed() {
                return Ok(running.submit.clone());
            }
            // Supervisor died; fall through and relaunch.
        }
        let running = spawn_supervisor(&self.inner.node_bin, &self.inner.script_path, self.inner.sig)?;
        let tx = running.submit.clone();
        *guard = Some(running);
        Ok(tx)
    }
}

fn spawn_supervisor(
    node_bin: &str,
    script_path: &std::path::Path,
    sig: &'static crate::lifecycle::child_signal::Signal,
) -> Result<Running> {
    if !script_path.exists() {
        return Err(anyhow!(
            "mathjax render script missing at {}",
            script_path.display()
        ));
    }
    let mut cmd = Command::new(node_bin);
    cmd.arg(script_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut contained = sig
        .spawn(&mut cmd)
        .with_context(|| format!("spawn {node_bin} {}", script_path.display()))?;

    let stdin = contained
        .stdin
        .take()
        .context("mathjax child stdin missing")?;
    let stdout = contained
        .stdout
        .take()
        .context("mathjax child stdout missing")?;
    let stderr = contained
        .stderr
        .take()
        .context("mathjax child stderr missing")?;

    let (submit_tx, submit_rx) = mpsc::channel::<Submission>(64);
    let (ends, ended) = oneshot::channel();

    // Stderr drain — pure logging, never on the result path.
    tokio::spawn(async move {
        let mut reader = sot_protocol::child_lines::ChildLines::new(stderr);
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::debug!(target: "mathjax.stderr", "{line}");
        }
    });

    tokio::spawn(supervisor_task(contained, stdin, stdout, submit_rx, ended, sig));
    Ok(Running { submit: submit_tx, _ends: ends })
}

async fn supervisor_task(
    mut contained: crate::lifecycle::child_signal::Contained,
    mut stdin: ChildStdin,
    stdout: tokio::process::ChildStdout,
    mut submit_rx: mpsc::Receiver<Submission>,
    ended: oneshot::Receiver<()>,
    sig: &'static crate::lifecycle::child_signal::Signal,
) {
    let mut pending: HashMap<u64, oneshot::Sender<Result<RenderedSvg>>> = HashMap::new();
    let mut next_id: u64 = 1;
    let mut stdout_lines = sot_protocol::child_lines::ChildLines::new(stdout);

    // The service races the slot: emptying it (a render past its bound) ends
    // this child even while the service waits on it, a write to a child that
    // reads nothing included.
    let serve = async {
        loop {
            tokio::select! {
                biased;
                // The daemon is shutting down: the signal has already killed the
                // child's tree.
                _ = sig.fired() => {
                    break;
                }
                // Drain incoming submissions, write to child stdin.
                sub = submit_rx.recv() => {
                    let Some(sub) = sub else {
                        // Every sender is gone: the same closeout as a release.
                        break;
                    };
                    let id = next_id;
                    next_id += 1;
                    let req = WireRequest { id, tex: &sub.latex, display: sub.display };
                    let line = match serde_json::to_string(&req) {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = sub.reply.send(Err(anyhow!("mathjax serialize: {e}")));
                            continue;
                        }
                    };
                    if let Err(e) = stdin.write_all(line.as_bytes()).await {
                        let _ = sub.reply.send(Err(anyhow!("mathjax stdin: {e}")));
                        break;
                    }
                    if let Err(e) = stdin.write_all(b"\n").await {
                        let _ = sub.reply.send(Err(anyhow!("mathjax stdin: {e}")));
                        break;
                    }
                    if let Err(e) = stdin.flush().await {
                        let _ = sub.reply.send(Err(anyhow!("mathjax stdin flush: {e}")));
                        break;
                    }
                    pending.insert(id, sub.reply);
                }
                // Read replies from child stdout.
                line = stdout_lines.next_line() => {
                    match line {
                        Ok(Some(line)) => route_response(&line, &mut pending),
                        Ok(None) => {
                            tracing::warn!("mathjax child stdout closed");
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "mathjax stdout error");
                            break;
                        }
                    }
                }
            }
        }
    };
    tokio::select! {
        biased;
        _ = ended => tracing::warn!("mathjax child released by its handle; ending it"),
        () = serve => {}
    }

    // Drain any in-flight requests with an error; supervisor exits and the
    // next caller triggers a fresh spawn via `ensure_supervisor`.
    for (_id, reply) in pending.drain() {
        let _ = reply.send(Err(anyhow!("mathjax sidecar terminated")));
    }
    let _ = contained.kill().await;
}

fn route_response(
    line: &str,
    pending: &mut HashMap<u64, oneshot::Sender<Result<RenderedSvg>>>,
) {
    let parsed: Result<WireResponse, _> = serde_json::from_str(line);
    let resp = match parsed {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %sot_protocol::codec::unparsed(&e, line.len()), "mathjax response parse failed");
            return;
        }
    };
    let Some(reply) = pending.remove(&resp.id) else {
        // id=0 from the sidecar means "unattributed"; just log.
        tracing::debug!(id = resp.id, "mathjax response without pending request");
        return;
    };
    if let Some(err) = resp.error {
        let _ = reply.send(Err(anyhow!("mathjax: {err}")));
    } else if let Some(svg) = resp.svg {
        let _ = reply.send(Ok(RenderedSvg {
            svg: svg.into_bytes(),
            ex: resp.ex.unwrap_or(8.0),
        }));
    } else {
        let _ = reply.send(Err(anyhow!("mathjax: missing svg and error fields")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A render the child never answers is an error at `RENDER_BOUND` and ends that child, and the next render starts
    /// a fresh one. The stand-in for node is `sh` running a script that reads and never answers.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_render_past_its_bound_is_an_error_and_ends_its_child() {
        use crate::sidecars::contract_tests::within;
        use std::time::Duration;
        let dir = tempfile::Builder::new().prefix("sot-mathjax-bound-").tempdir().unwrap();
        let script = dir.path().join("render.sh");
        let starts = dir.path().join("starts");
        std::fs::write(&script, format!("echo started >> '{}'\nwhile read -r line; do :; done\n", starts.display()))
            .unwrap();
        let sig: &'static crate::lifecycle::child_signal::Signal =
            Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let mathjax = MathJax {
            inner: Arc::new(MathJaxInner {
                script_path: script,
                node_bin: "/bin/sh".to_string(),
                submit: Mutex::new(None),
                sig,
            }),
        };
        let started_lines = || std::fs::read_to_string(&starts).map(|s| s.lines().count()).unwrap_or(0);

        let t0 = std::time::Instant::now();
        let err = tokio::time::timeout(Duration::from_secs(30), mathjax.render("x", false))
            .await
            .expect("the render is still waiting at 30 s")
            .expect_err("a render the child never answers");
        let took = t0.elapsed();
        let bound = Duration::from_secs(10); // RENDER_BOUND, written out so this test also runs on the unbounded code
        assert!(took >= bound && took < bound + Duration::from_secs(5), "ended after {took:?}");
        assert!(format!("{err:#}").contains("timed out after 10s"), "{err:#}");
        within(Duration::from_secs(5), "the timed-out child is ended", || sig.live() == 0).await;
        assert_eq!(started_lines(), 1);

        let next = {
            let mathjax = mathjax.clone();
            tokio::spawn(async move { mathjax.render("y", false).await })
        };
        within(Duration::from_secs(5), "the next render starts a fresh child", || {
            sig.live() == 1 && started_lines() == 2
        })
        .await;
        println!("mathjax-bound: {err:#} after {took:?}; the child ended; the next render started child {}", started_lines());
        next.abort();
        drop(mathjax);
        within(Duration::from_secs(5), "the fresh child ends with its handle", || sig.live() == 0).await;
    }

    /// ADR 0049, User isolation: a MathJax line that does not parse is logged by position and length, never its bytes.
    #[test]
    fn a_mathjax_line_that_does_not_parse_is_logged_without_its_bytes() {
        let logged = crate::sidecars::logged_by(|| {
            let mut pending = HashMap::new();
            route_response(r#"{"id":2,"svg":"<svg data-u="http://127.0.0.1:41234/0123456789ab"#, &mut pending);
        });
        assert_eq!(logged.lines().filter(|l| l.contains("WARN")).count(), 1, "{logged}");
        assert!(!logged.contains("0123") && !logged.contains("41234"), "{logged}");
    }
}
