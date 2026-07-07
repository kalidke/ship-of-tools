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
// stdin/stdout, routes responses by request id, drains in-flight requests
// with an error on child death, and relaunches lazily on the next call —
// same lifecycle pattern as `mathjax.rs`.
//
// Spawn command:
//   julia --project=<repo>/julia/kernel \
//         -e 'using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout; project_root="<root>")'

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

/// Max time to await a kernel response before giving up so the caller can fall
/// back (e.g. `preview.get` → bytes-level reader). Bounds the blast radius of a
/// hung / crashing / cold kernel: `handle_preview_get` awaits `request()`
/// INLINE on the per-connection task (server.rs), so an unbounded wait freezes
/// the whole frontend connection — not just the one preview — for as long as
/// the kernel takes to respond or die. We observed a crash-looping kernel
/// (stale post-rename env) block previews ~50s each until its stdout EOF'd.
/// 10s comfortably covers a warm kernel's slowest introspection op and a cold
/// `using` precompile on (re)spawn, while capping a stuck kernel at 10s. Same
/// "bound every cross-process wait" principle as ADR-0027's write-timeout.
const KERNEL_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Wire-contract protocol version the backend expects the Julia kernel to speak
/// (ADR 0030 §2). Mirrors `ShipToolsKernel.PROTOCOL_VERSION`. The BE and the
/// Julia bundle ship as a unit, so a mismatch is a belt-and-suspenders signal
/// (a stale kernel checkout) rather than a supported configuration — we log it
/// loudly at kernel hello but do NOT kill the kernel.
const KERNEL_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone)]
pub struct Kernel {
    inner: Arc<KernelInner>,
}

struct KernelInner {
    /// Absolute path of the kernel's Julia project (i.e. the directory
    /// holding the Project.toml). Passed to `julia --project=…`.
    kernel_project: PathBuf,
    /// Absolute path the kernel walks for `file.parse`-style ops with
    /// relative paths — the actual project being explored, not the kernel
    /// package itself.
    project_root: PathBuf,
    /// `julia` binary, overridable via SOT_JULIA_BIN for unusual installs.
    julia_bin: String,
    /// Submission channel for the supervisor task. When closed, the next
    /// caller respawns.
    submit: Mutex<Option<mpsc::Sender<Submission>>>,
}

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
        let julia_bin = std::env::var("SOT_JULIA_BIN").unwrap_or_else(|_| "julia".to_string());
        Self {
            inner: Arc::new(KernelInner {
                kernel_project,
                project_root,
                julia_bin,
                submit: Mutex::new(None),
            }),
        }
    }

    /// Default kernel project location — `julia/kernel` resolved for both
    /// layouts (dev checkout / release install) via `paths::resource_dir`
    /// (ADR 0030 §4).
    pub fn default_kernel_project() -> PathBuf {
        crate::paths::resource_dir("julia/kernel")
    }

    /// Send a request to the kernel and wait for the matching response.
    /// `op` is the kernel-side verb (e.g. `kernel.hello`, `modules.list`,
    /// `file.parse`); `payload` is the kernel's payload shape.
    pub async fn request(&self, op: &str, payload: Value) -> Result<Value> {
        let tx = self.ensure_supervisor().await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Submission {
            op: op.to_string(),
            payload,
            reply: reply_tx,
        })
        .await
        .map_err(|_| anyhow!("kernel supervisor channel closed"))?;
        // Bound the wait (see KERNEL_REQUEST_TIMEOUT): a hung / crashing / cold
        // kernel must not block the caller — and, since the preview handler
        // awaits this inline on the connection task, the whole FE connection —
        // until the kernel finally responds or dies. On timeout we drop
        // reply_rx (the supervisor's later `reply.send` becomes a harmless
        // no-op) and return an error so the caller falls back.
        match tokio::time::timeout(KERNEL_REQUEST_TIMEOUT, reply_rx).await {
            Ok(r) => r.map_err(|_| anyhow!("kernel supervisor dropped reply channel"))?,
            Err(_) => Err(anyhow!(
                "kernel request '{op}' timed out after {KERNEL_REQUEST_TIMEOUT:?}; \
                 kernel hung or still starting"
            )),
        }
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
            &self.inner.kernel_project,
            &self.inner.project_root,
        )?;
        *guard = Some(tx.clone());
        // ADR 0030 §2: validate the kernel's protocol once per (re)spawn. Runs
        // detached so it doesn't stall the caller that triggered the spawn; the
        // supervisor's request queue serializes it ahead of the real op.
        spawn_kernel_hello_check(tx.clone());
        Ok(tx)
    }
}

/// Fire a one-shot `kernel.hello` at a freshly-spawned kernel and validate its
/// reported `protocol` against `KERNEL_PROTOCOL_VERSION` (ADR 0030 §2). Detached
/// and non-fatal: a mismatch is logged at ERROR (the kernel keeps running,
/// since BE + kernel ship as a unit and a live-but-skewed kernel is still more
/// useful than a dead one), a missing `protocol` field (pre-ADR-0030 kernel) is
/// a WARN, and a hello that never lands (cold/crashing kernel) is a WARN — the
/// next real request respawns and re-checks.
fn spawn_kernel_hello_check(tx: mpsc::Sender<Submission>) {
    tokio::spawn(async move {
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(Submission {
                op: "kernel.hello".to_string(),
                payload: serde_json::json!({}),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            // Supervisor already gone before we could ask — nothing to validate.
            return;
        }
        let payload = match tokio::time::timeout(KERNEL_REQUEST_TIMEOUT, reply_rx).await {
            Ok(Ok(Ok(v))) => v,
            _ => {
                tracing::warn!(
                    "kernel protocol check: no `kernel.hello` response (kernel cold, \
                     crashing, or predates the op) — will re-check on next respawn"
                );
                return;
            }
        };
        let kernel_version = payload.get("version").and_then(|v| v.as_str()).unwrap_or("?");
        match payload.get("protocol").and_then(|v| v.as_u64()) {
            Some(p) if p as u32 == KERNEL_PROTOCOL_VERSION => {
                tracing::info!(
                    kernel_protocol = p,
                    %kernel_version,
                    "kernel hello ok (protocol matches)"
                );
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
    });
}

fn spawn_supervisor(
    julia_bin: &str,
    kernel_project: &Path,
    project_root: &Path,
) -> Result<mpsc::Sender<Submission>> {
    if !kernel_project.exists() {
        return Err(anyhow!(
            "kernel project missing at {}",
            kernel_project.display()
        ));
    }
    let project_root_str = project_root
        .to_str()
        .ok_or_else(|| anyhow!("project_root not utf-8: {}", project_root.display()))?;
    // Escape any backslashes / quotes in the path before embedding it in the
    // Julia source. The escape set is small: `\` and `"`.
    let escaped = project_root_str
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let julia_src = format!(
        "using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout; project_root=\"{escaped}\")"
    );

    let mut child: Child = Command::new(julia_bin)
        .arg(format!("--project={}", kernel_project.display()))
        .arg("-e")
        .arg(&julia_src)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {julia_bin} --project={}", kernel_project.display()))?;

    let stdin = child.stdin.take().context("kernel child stdin missing")?;
    let stdout = child.stdout.take().context("kernel child stdout missing")?;
    let stderr = child.stderr.take().context("kernel child stderr missing")?;

    let (submit_tx, submit_rx) = mpsc::channel::<Submission>(64);

    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::debug!(target: "kernel.stderr", "{line}");
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
    let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
    let mut next_id: u64 = 1;
    let mut stdout_lines = BufReader::new(stdout).lines();

    loop {
        tokio::select! {
            biased;
            sub = submit_rx.recv() => {
                let Some(sub) = sub else {
                    drop(stdin);
                    let _ = child.wait().await;
                    return;
                };
                let id = next_id;
                next_id += 1;
                let req = WireRequest {
                    v: 1,
                    id,
                    kind: "req",
                    op: &sub.op,
                    payload: &sub.payload,
                };
                let line = match serde_json::to_string(&req) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = sub.reply.send(Err(anyhow!("kernel serialize: {e}")));
                        continue;
                    }
                };
                if let Err(e) = stdin.write_all(line.as_bytes()).await {
                    let _ = sub.reply.send(Err(anyhow!("kernel stdin: {e}")));
                    break;
                }
                if let Err(e) = stdin.write_all(b"\n").await {
                    let _ = sub.reply.send(Err(anyhow!("kernel stdin: {e}")));
                    break;
                }
                if let Err(e) = stdin.flush().await {
                    let _ = sub.reply.send(Err(anyhow!("kernel stdin flush: {e}")));
                    break;
                }
                pending.insert(id, sub.reply);
            }
            line = stdout_lines.next_line() => {
                match line {
                    Ok(Some(line)) => route_response(&line, &mut pending),
                    Ok(None) => {
                        tracing::warn!("kernel child stdout closed");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "kernel stdout error");
                        break;
                    }
                }
            }
        }
    }

    for (_id, reply) in pending.drain() {
        let _ = reply.send(Err(anyhow!("kernel terminated")));
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
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
