// mathjax.rs — supervisor for the MathJax-SVG sidecar.
//
// Per ADR 0012 the Ship of Tools frontend renders inline LaTeX as MathJax-SVG.
// The actual rendering happens in a long-lived `node` process running
// `rust/backend/sidecars/mathjax/render.mjs`; this module owns that
// child, multiplexes Rust callers onto its single stdin/stdout, routes
// responses back by request id.
//
// API: `MathJax::start(...)` returns a cheap clone-able handle. Callers
// `.render(latex, display).await` for SVG bytes. On child death the
// supervisor drains in-flight oneshots with an error, then lazily
// relaunches on the next request.
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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
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
    /// Submission channel. Holds a `Sender` for the supervisor task; the
    /// task itself owns the child + state. When the supervisor exits, this
    /// channel breaks and we relaunch lazily on the next call.
    submit: Mutex<Option<mpsc::Sender<Submission>>>,
}

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
    pub fn new(script_path: PathBuf) -> Self {
        let node_bin = std::env::var("SOT_NODE_BIN").unwrap_or_else(|_| "node".to_string());
        Self {
            inner: Arc::new(MathJaxInner {
                script_path,
                node_bin,
                submit: Mutex::new(None),
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

    /// Convert LaTeX to SVG via the sidecar. Spawns the child on first call,
    /// reuses across subsequent calls, relaunches on death.
    pub async fn render(&self, latex: &str, display: bool) -> Result<RenderedSvg> {
        let tx = self.ensure_supervisor().await?;
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
    }

    async fn ensure_supervisor(&self) -> Result<mpsc::Sender<Submission>> {
        let mut guard = self.inner.submit.lock().await;
        if let Some(tx) = guard.as_ref() {
            if !tx.is_closed() {
                return Ok(tx.clone());
            }
            // Supervisor died; fall through and relaunch.
        }
        let tx = spawn_supervisor(&self.inner.node_bin, &self.inner.script_path)?;
        *guard = Some(tx.clone());
        Ok(tx)
    }
}

fn spawn_supervisor(node_bin: &str, script_path: &std::path::Path) -> Result<mpsc::Sender<Submission>> {
    if !script_path.exists() {
        return Err(anyhow!(
            "mathjax render script missing at {}",
            script_path.display()
        ));
    }
    let mut child: Child = Command::new(node_bin)
        .arg(script_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {node_bin} {}", script_path.display()))?;

    let stdin = child
        .stdin
        .take()
        .context("mathjax child stdin missing")?;
    let stdout = child
        .stdout
        .take()
        .context("mathjax child stdout missing")?;
    let stderr = child
        .stderr
        .take()
        .context("mathjax child stderr missing")?;

    let (submit_tx, submit_rx) = mpsc::channel::<Submission>(64);

    // Stderr drain — pure logging, never on the result path.
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::debug!(target: "mathjax.stderr", "{line}");
        }
    });

    tokio::spawn(supervisor_task(child, stdin, stdout, submit_rx));
    Ok(submit_tx)
}

async fn supervisor_task(
    mut child: Child,
    mut stdin: ChildStdin,
    stdout: tokio::process::ChildStdout,
    mut submit_rx: mpsc::Receiver<Submission>,
) {
    let mut pending: HashMap<u64, oneshot::Sender<Result<RenderedSvg>>> = HashMap::new();
    let mut next_id: u64 = 1;
    let mut stdout_lines = BufReader::new(stdout).lines();

    loop {
        tokio::select! {
            biased;
            // Drain incoming submissions, write to child stdin.
            sub = submit_rx.recv() => {
                let Some(sub) = sub else {
                    // No more callers — close stdin, await child, exit.
                    drop(stdin);
                    let _ = child.wait().await;
                    return;
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

    // Drain any in-flight requests with an error; supervisor exits and the
    // next caller triggers a fresh spawn via `ensure_supervisor`.
    for (_id, reply) in pending.drain() {
        let _ = reply.send(Err(anyhow!("mathjax sidecar terminated")));
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

fn route_response(
    line: &str,
    pending: &mut HashMap<u64, oneshot::Sender<Result<RenderedSvg>>>,
) {
    let parsed: Result<WireResponse, _> = serde_json::from_str(line);
    let resp = match parsed {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, line, "mathjax response parse failed");
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
