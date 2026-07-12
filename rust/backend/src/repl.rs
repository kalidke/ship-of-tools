// repl.rs — supervisor for the persistent Julia REPL child.
//
// Per ADR 0009 the REPL is a separate `julia` process the backend keeps
// alive. The frontend sends code to evaluate; the REPL responds with a
// list of frames (stdout / stderr / value / error / done). Phase-1
// implementation is synchronous-collect — one response carries all frames
// for that eval. Streamed event-frame delivery (so the chrome can show
// stdout as it arrives) is phase 2; the protocol surface is the same
// either way, just the timing changes.
//
// Lifecycle pattern mirrors `kernel.rs` and `mathjax.rs`: long-lived
// child, mpsc submit channel, oneshot replies, drain on death + relaunch
// on next call.
//
// Distinct from the Julia kernel (`kernel.rs`): the kernel introspects
// project state without running user code (Modules-mode, AST hashes); the
// REPL evaluates arbitrary user code in its own `Main` namespace. They're
// separate so a runaway eval can't take down kernel introspection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

/// One streamed REPL frame relayed off the supervisor onto the per-backend
/// broadcast bus. The supervisor reads each `repl.frame` evt line off the
/// Julia child's stdout and fans it out here; every connection subscribes and
/// writes a `repl.frame` evt frame. Mirrors `workspaces::AgentMessage` — a
/// small Clone+Debug payload type over a `broadcast::channel`. `workspace_id`
/// is the originating workspace (None = the legacy singleton REPL). `frame` is
/// the opaque `{kind, ...}` object the Julia shim emitted, passed through
/// verbatim so the protocol surface stays kernel-defined.
#[derive(Clone, Debug)]
pub struct ReplFrameMsg {
    pub eval_id: u64,
    pub workspace_id: Option<String>,
    pub frame: serde_json::Value,
}

/// Inline stdout+stderr byte budget for a collected `repl.execute` run. Past
/// this, further stdout/stderr text frames are dropped and `truncated` is set —
/// but value/image/error/done frames are always kept. Bounds memory against a
/// runaway `println` loop and keeps the response under the 1 MiB envelope cap.
pub const EXEC_TEXT_CAP: usize = 256 * 1024;

/// Loss-free per-run frame sink for `repl.execute`. The supervisor tees each
/// frame for a collected eval_id in here (in addition to the best-effort
/// broadcast bus), so a slow consumer can never `Lagged`-drop a frame the way
/// the shared 256-slot `broadcast` would. Bounded by `EXEC_TEXT_CAP`.
#[derive(Default)]
pub struct ExecAccum {
    pub frames: Vec<serde_json::Value>,
    pub text_bytes: usize,
    pub truncated: bool,
}

impl ExecAccum {
    fn push(&mut self, frame: serde_json::Value) {
        let kind = frame.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind == "stdout" || kind == "stderr" {
            if self.text_bytes >= EXEC_TEXT_CAP {
                self.truncated = true;
                return;
            }
            let len = frame
                .get("text")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or(0);
            self.text_bytes += len;
            if self.text_bytes >= EXEC_TEXT_CAP {
                self.truncated = true;
            }
        }
        self.frames.push(frame);
    }
}

pub type ExecCollector = Arc<std::sync::Mutex<ExecAccum>>;

#[derive(Clone)]
pub struct Repl {
    inner: Arc<ReplInner>,
}

struct ReplInner {
    repl_project: PathBuf,
    julia_bin: String,
    submit: Mutex<Option<mpsc::Sender<Submission>>>,
    /// Broadcast sink for streamed `repl.frame` evts. Threaded into every
    /// supervisor we spawn (initial + each `restart_with_project`) so frames
    /// from a fresh child still reach subscribers.
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    /// The originating workspace id, stamped onto every `ReplFrameMsg` so the
    /// frontend can route frames to the right REPL drawer. None for the legacy
    /// singleton REPL.
    workspace_id: Option<String>,
}

struct Submission {
    op: String,
    payload: Value,
    /// `Some` for request/response ops (interrupt, execute): the supervisor
    /// completes it with the terminal res payload. `None` for fire-and-forget
    /// evals: the supervisor does NOT track them in `pending` — completion is
    /// keyed off the streamed `done` frame on the broadcast bus, and the
    /// terminal res ack is dropped.
    reply: Option<oneshot::Sender<Result<Value>>>,
    /// `Some` for `repl.execute`: the supervisor tees every frame for this
    /// submission's eval_id into the collector, loss-free, so the handler can
    /// return the full collected output. `None` for every other op.
    collector: Option<ExecCollector>,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    v: u32,
    id: u64,
    kind: &'a str,
    op: &'a str,
    payload: &'a Value,
}

/// One line off the REPL child's stdout. Both the streamed `repl.frame` evts
/// and the terminal res ack share this envelope shape; `kind`/`op` disambiguate
/// them. `kind`/`op` default to `""` so a malformed line still deserializes far
/// enough to be logged-and-dropped rather than crashing the parse.
#[derive(Deserialize)]
struct WireEnvelope {
    id: u64,
    #[serde(default)]
    #[allow(dead_code)]
    kind: String,
    #[serde(default)]
    op: String,
    payload: Value,
}

impl Repl {
    pub fn new(
        repl_project: PathBuf,
        frame_tx: broadcast::Sender<ReplFrameMsg>,
        workspace_id: Option<String>,
    ) -> Self {
        let julia_bin = std::env::var("SOT_JULIA_BIN").unwrap_or_else(|_| "julia".to_string());
        Self {
            inner: Arc::new(ReplInner {
                repl_project,
                julia_bin,
                submit: Mutex::new(None),
                frame_tx,
                workspace_id,
            }),
        }
    }

    pub fn default_repl_project() -> PathBuf {
        // Both layouts (dev checkout / release install) — ADR 0030 §4.
        crate::paths::resource_dir("julia/repl")
    }

    /// Request/response op: queue the submission with a reply channel and
    /// await the terminal res payload. Used by `repl.interrupt`, which stays a
    /// simple req→one-res exchange. (Eval/run_file are now fire-and-forget via
    /// `submit`; their frames stream over the broadcast bus instead.)
    pub async fn request(&self, op: &str, payload: Value) -> Result<Value> {
        let tx = self.ensure_supervisor().await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Submission {
            op: op.to_string(),
            payload,
            reply: Some(reply_tx),
            collector: None,
        })
        .await
        .map_err(|_| anyhow!("repl supervisor channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("repl supervisor dropped reply channel"))?
    }

    /// Execute op (`repl.execute`, ADR 0032): submit WITH both a reply channel
    /// (to capture the shim's authoritative terminal `res`) AND a frame
    /// collector (to gather the run's frames loss-free off the supervisor,
    /// bypassing the lossy broadcast bus). Returns immediately with the reply
    /// receiver and the shared collector; the caller awaits the res under its
    /// own timeout, then reads the collector. Frame ordering guarantees all
    /// frames precede the terminal res on the child's stdout, so by the time
    /// the res arrives the collector holds the complete output.
    pub async fn execute(
        &self,
        op: &str,
        payload: Value,
    ) -> Result<(oneshot::Receiver<Result<Value>>, ExecCollector)> {
        let tx = self.ensure_supervisor().await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let collector: ExecCollector = Arc::new(std::sync::Mutex::new(ExecAccum::default()));
        tx.send(Submission {
            op: op.to_string(),
            payload,
            reply: Some(reply_tx),
            collector: Some(collector.clone()),
        })
        .await
        .map_err(|_| anyhow!("repl supervisor channel closed"))?;
        Ok((reply_rx, collector))
    }

    /// Fire-and-forget op: queue the submission with no reply channel and
    /// return once it's enqueued. The supervisor does NOT insert it into
    /// `pending`; its streamed `repl.frame` evts (including the terminal
    /// `done` frame) are fanned out over the broadcast bus, and the terminal
    /// res ack line is dropped. Used by `repl.eval` / `repl.run_file` so the
    /// connection loop never blocks waiting for an eval to finish (a mid-eval
    /// `repl.interrupt` must still be readable).
    pub async fn submit(&self, op: &str, payload: Value) -> Result<()> {
        let tx = self.ensure_supervisor().await?;
        tx.send(Submission {
            op: op.to_string(),
            payload,
            reply: None,
            collector: None,
        })
        .await
        .map_err(|_| anyhow!("repl supervisor channel closed"))?;
        Ok(())
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
            &self.inner.repl_project,
            self.inner.frame_tx.clone(),
            self.inner.workspace_id.clone(),
        )?;
        *guard = Some(tx.clone());
        Ok(tx)
    }

    /// Tear down the persistent REPL child and respawn it with `user_project`
    /// active (`julia --project=<user_project>`). Used by the `r` keybind in
    /// the frontend (priority J): "reset and run" walks up from the file to
    /// find the closest `Project.toml`, calls this, then forwards a plain
    /// `repl.run_file { fresh: false }` to the fresh child.
    ///
    /// The supervisor's stdin handle is held by `supervisor_task`. Dropping
    /// the submit sender closes `submit_rx`, the task's `recv` returns
    /// `None`, the task drops its stdin handle, and the Julia child exits
    /// on EOF. We don't `await` the task's JoinHandle (we never stored one)
    /// — instead we re-spawn immediately under the same lock so callers
    /// blocking on this method see the new sender. Any in-flight requests
    /// against the old child are reaped by `supervisor_task`'s drain loop.
    pub async fn restart_with_project(&self, user_project: &Path) -> Result<()> {
        let mut guard = self.inner.submit.lock().await;
        // Drop the existing sender (if any). This closes the channel, which
        // is what causes the supervisor_task to terminate and the child to
        // exit. We do NOT await the old task here — it cleans up
        // asynchronously and the next request will go to the new child via
        // the freshly-installed sender below.
        guard.take();
        let tx = spawn_supervisor_with_project(
            &self.inner.julia_bin,
            &self.inner.repl_project,
            user_project,
            self.inner.frame_tx.clone(),
            self.inner.workspace_id.clone(),
        )?;
        *guard = Some(tx);
        Ok(())
    }
}

fn spawn_supervisor(
    julia_bin: &str,
    repl_project: &Path,
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    workspace_id: Option<String>,
) -> Result<mpsc::Sender<Submission>> {
    if !repl_project.exists() {
        return Err(anyhow!(
            "repl project missing at {}",
            repl_project.display()
        ));
    }
    let julia_src = "using ShipToolsRepl; ShipToolsRepl.serve(stdin, stdout)";

    let mut child: Child = Command::new(julia_bin)
        .arg(format!("--project={}", repl_project.display()))
        .arg("-e")
        .arg(julia_src)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {julia_bin} --project={}", repl_project.display()))?;

    let stdin = child.stdin.take().context("repl child stdin missing")?;
    let stdout = child.stdout.take().context("repl child stdout missing")?;
    let stderr = child.stderr.take().context("repl child stderr missing")?;

    let (submit_tx, submit_rx) = mpsc::channel::<Submission>(16);

    let stderr_tail = spawn_stderr_tail(stderr);

    tokio::spawn(supervisor_task(
        child, stdin, stdout, submit_rx, frame_tx, workspace_id, stderr_tail,
    ));
    Ok(submit_tx)
}

/// Like `spawn_supervisor` but activates `user_project` for user code while
/// keeping `ShipToolsRepl` reachable for the dispatch loop. Used by
/// `Repl::restart_with_project` to bounce the persistent REPL into the
/// project closest to a `.jl` file the user is about to run.
///
/// We can't pass `--project=<user_project>` *and* expect `using ShipToolsRepl`
/// to resolve — the REPL shim isn't in the user's manifest. The standard
/// trick is `JULIA_LOAD_PATH=<repl_project>:` so `Base.load_path()` searches
/// the REPL project (where `ShipToolsRepl` lives) alongside the user project
/// (selected via `--project`). The trailing colon preserves the rest of the
/// default load path (stdlib, etc.) so `using` of standard packages still
/// works inside the user code.
fn spawn_supervisor_with_project(
    julia_bin: &str,
    repl_project: &Path,
    user_project: &Path,
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    workspace_id: Option<String>,
) -> Result<mpsc::Sender<Submission>> {
    if !repl_project.exists() {
        return Err(anyhow!(
            "repl project missing at {}",
            repl_project.display()
        ));
    }
    let julia_src = "using ShipToolsRepl; ShipToolsRepl.serve(stdin, stdout)";

    // JULIA_LOAD_PATH uses ':' on Unix and ';' on Windows. Use the
    // platform-appropriate separator so the shim project is reachable on
    // both. Trailing separator preserves the default `["@", "@v#.#", "@stdlib"]`
    // entries (which become `@`, etc. via the empty token).
    #[cfg(windows)]
    let sep = ";";
    #[cfg(not(windows))]
    let sep = ":";
    let load_path = format!("{}{sep}", repl_project.display());

    let mut child: Child = Command::new(julia_bin)
        .env("JULIA_LOAD_PATH", &load_path)
        .arg(format!("--project={}", user_project.display()))
        .arg("-e")
        .arg(julia_src)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "spawn {julia_bin} --project={} (JULIA_LOAD_PATH={load_path})",
                user_project.display()
            )
        })?;

    let stdin = child.stdin.take().context("repl child stdin missing")?;
    let stdout = child.stdout.take().context("repl child stdout missing")?;
    let stderr = child.stderr.take().context("repl child stderr missing")?;

    let (submit_tx, submit_rx) = mpsc::channel::<Submission>(16);

    let stderr_tail = spawn_stderr_tail(stderr);

    tokio::spawn(supervisor_task(
        child, stdin, stdout, submit_rx, frame_tx, workspace_id, stderr_tail,
    ));
    Ok(submit_tx)
}

/// Spawn the stderr reader: per-line DEBUG (healthy julia is chatty), plus a
/// bounded tail the supervisor dumps at WARN when the child DIES — the
/// 2026-07-03 stale-Manifest incident died with its only evidence at debug
/// level, and the REPL just silently "didn't work".
fn spawn_stderr_tail(
    stderr: tokio::process::ChildStderr,
) -> std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>> {
    let tail: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let writer = tail.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::debug!(target: "repl.stderr", "{line}");
            let mut t = writer.lock().unwrap_or_else(|e| e.into_inner());
            if t.len() >= 30 {
                t.pop_front();
            }
            t.push_back(line);
        }
    });
    tail
}

async fn supervisor_task(
    mut child: Child,
    mut stdin: ChildStdin,
    stdout: tokio::process::ChildStdout,
    mut submit_rx: mpsc::Receiver<Submission>,
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    workspace_id: Option<String>,
    stderr_tail: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
) {
    let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
    // Streamed (fire-and-forget) evals in flight: eval_id recorded at submit,
    // cleared when its `done` frame routes. On child death each survivor gets
    // synthetic error+done frames so the FE'S in-flight entry CLOSES — before
    // this, a dying child left evals hanging forever with no visible cause.
    let mut streaming: std::collections::HashSet<u64> = std::collections::HashSet::new();
    // Active `repl.execute` collectors, keyed by eval_id (frames carry eval_id).
    let mut collectors: HashMap<u64, ExecCollector> = HashMap::new();
    // Outgoing wire id -> eval_id, so the terminal res can drop the collector
    // even when the shim emits no `done` frame (the missing-file res path).
    let mut collector_ids: HashMap<u64, u64> = HashMap::new();
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
                        if let Some(reply) = sub.reply {
                            let _ = reply.send(Err(anyhow!("repl serialize: {e}")));
                        }
                        continue;
                    }
                };
                if let Err(e) = stdin.write_all(line.as_bytes()).await {
                    if let Some(reply) = sub.reply {
                        let _ = reply.send(Err(anyhow!("repl stdin: {e}")));
                    }
                    break;
                }
                if let Err(e) = stdin.write_all(b"\n").await {
                    if let Some(reply) = sub.reply {
                        let _ = reply.send(Err(anyhow!("repl stdin: {e}")));
                    }
                    break;
                }
                if let Err(e) = stdin.flush().await {
                    if let Some(reply) = sub.reply {
                        let _ = reply.send(Err(anyhow!("repl stdin flush: {e}")));
                    }
                    break;
                }
                let eid = sub.payload.get("eval_id").and_then(Value::as_u64);
                // `repl.execute` submissions carry a collector: tee this
                // eval_id's frames into it loss-free (in addition to `pending`,
                // which captures the terminal res).
                if let Some(collector) = sub.collector {
                    if let Some(eid) = eid {
                        collectors.insert(eid, collector);
                        collector_ids.insert(id, eid);
                    }
                }
                // Only request/response ops are tracked in `pending`. A
                // fire-and-forget submission (`reply == None`) streams its
                // frames over the broadcast bus; record its eval_id so a
                // child death can close it out with synthetic frames.
                if let Some(reply) = sub.reply {
                    pending.insert(id, reply);
                } else if let Some(eid) = eid {
                    streaming.insert(eid);
                }
            }
            line = stdout_lines.next_line() => {
                match line {
                    Ok(Some(line)) => route_line(
                        &line,
                        &mut pending,
                        &mut streaming,
                        &mut collectors,
                        &mut collector_ids,
                        &frame_tx,
                        &workspace_id,
                    ),
                    Ok(None) => {
                        tracing::warn!("repl child stdout closed");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "repl stdout error");
                        break;
                    }
                }
            }
        }
    }

    for (_id, reply) in pending.drain() {
        let _ = reply.send(Err(anyhow!("repl terminated")));
    }
    // The child is gone: make the failure VISIBLE (stderr tail at WARN — its
    // death cry was previously debug-only) and CLOSED-OUT (synthetic
    // error+done frames per in-flight streamed eval, so the FE's entries
    // resolve instead of hanging forever — the 2026-07-03 "REPL not working").
    let tail: Vec<String> = stderr_tail
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect();
    if !tail.is_empty() {
        tracing::warn!(workspace = ?workspace_id, "repl child died; last stderr:\n{}", tail.join("\n"));
    }
    for eid in streaming.drain() {
        let _ = frame_tx.send(ReplFrameMsg {
            eval_id: eid,
            workspace_id: workspace_id.clone(),
            frame: serde_json::json!({
                "kind": "error",
                "message": "REPL process died mid-eval — it respawns on your next eval; the backend log has its stderr tail",
                "stacktrace": [],
            }),
        });
        let _ = frame_tx.send(ReplFrameMsg {
            eval_id: eid,
            workspace_id: workspace_id.clone(),
            frame: serde_json::json!({ "kind": "done", "eval_id": eid, "elapsed_ms": 0 }),
        });
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Route one stdout line off the REPL child. A `repl.frame` evt is fanned out
/// over the broadcast bus (its payload is `{eval_id, frame}`); any other
/// envelope is a terminal res. A res completes a tracked request/response op
/// (`pending`); for a fire-and-forget eval there's no `pending` entry, so the
/// ack is logged and dropped — completion is keyed off the streamed `done`
/// frame on the bus instead.
#[allow(clippy::too_many_arguments)]
fn route_line(
    line: &str,
    pending: &mut HashMap<u64, oneshot::Sender<Result<Value>>>,
    streaming: &mut std::collections::HashSet<u64>,
    collectors: &mut HashMap<u64, ExecCollector>,
    collector_ids: &mut HashMap<u64, u64>,
    frame_tx: &broadcast::Sender<ReplFrameMsg>,
    workspace_id: &Option<String>,
) {
    let env: WireEnvelope = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, line, "repl line parse failed");
            return;
        }
    };

    if env.op == "repl.frame" {
        // Streamed frame evt. Payload shape is `{eval_id, frame}`; pull both
        // out and fan the frame onto the broadcast bus. A missing eval_id is
        // skipped (malformed) rather than defaulted, so the frontend never
        // mis-attributes a frame.
        let eval_id = match env.payload.get("eval_id").and_then(Value::as_u64) {
            Some(id) => id,
            None => {
                tracing::warn!(line, "repl.frame evt missing eval_id; dropping");
                return;
            }
        };
        let frame = env
            .payload
            .get("frame")
            .cloned()
            .unwrap_or(Value::Null);
        let is_done = frame.get("kind").and_then(Value::as_str) == Some("done");
        if is_done {
            streaming.remove(&eval_id);
        }
        // Tee into a `repl.execute` collector, loss-free, before the frame goes
        // onto the best-effort broadcast bus. The `done` frame ends collection
        // for this eval_id (the res-side cleanup covers the no-`done` path).
        if let Some(collector) = collectors.get(&eval_id) {
            if let Ok(mut acc) = collector.lock() {
                acc.push(frame.clone());
            }
            if is_done {
                collectors.remove(&eval_id);
            }
        }
        let msg = ReplFrameMsg {
            eval_id,
            workspace_id: workspace_id.clone(),
            frame,
        };
        // Ignore send errors: a closed channel just means no connection is
        // currently subscribed, which is fine.
        let _ = frame_tx.send(msg);
        return;
    }

    // Terminal res ack. Drop any collector for this run first — this is the
    // authoritative completion signal and is the ONLY close-out for a run that
    // emits no `done` frame (e.g. run_file on a missing path).
    if let Some(eval_id) = collector_ids.remove(&env.id) {
        collectors.remove(&eval_id);
    }
    match pending.remove(&env.id) {
        Some(reply) => {
            let _ = reply.send(Ok(env.payload));
        }
        None => {
            // Fire-and-forget eval's terminal ack — expected, not an error.
            tracing::debug!(id = env.id, op = %env.op, "repl res for untracked id (fire-and-forget ack); dropping");
        }
    }
}
