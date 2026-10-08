//! The REPL child's task: spawning it, its wire to the child, routing its lines and closing out on death.

use std::collections::HashMap;
use std::process::Stdio;

use anyhow::Context;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};

use super::*;
use super::lifecycle::{browser_ports_key, lifecycle_begin_starting, lifecycle_transition};
use crate::sidecars::WireRequest;

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

/// The one REPL spawn recipe: activates `user_project` for user code (even a
/// bare workspace with no `Project.toml`) while keeping `ShipToolsRepl`
/// reachable for the dispatch loop. Initial start, death respawn and
/// `Repl::restart_with_project` all spawn through it.
///
/// We can't pass `--project=<user_project>` *and* expect `using ShipToolsRepl`
/// to resolve — the REPL shim isn't in the user's manifest. The standard
/// trick is `JULIA_LOAD_PATH=@:<repl_project>:` — the workspace project (`@` =
/// `--project`) FIRST so a dependency the workspace shares with the shim (e.g.
/// JSON3) resolves from the WORKSPACE's version, then the REPL project (where
/// `ShipToolsRepl` lives) as a fallback for the shim's own deps, then the
/// default load path (stdlib, etc.) via the trailing colon so `using` of
/// standard packages still works inside the user code.
pub(super) fn spawn_supervisor(
    user_project: &Path,
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    workspace_id: Option<String>,
    lifecycle: SharedLifecycle,
    sig: &'static crate::lifecycle::child_signal::Signal,
) -> Result<Supervisor> {
    let repl_project = Repl::repl_project();
    let (julia_bin, _) = crate::sidecars::julia::resolve_bin().map_err(|e| anyhow!(e))?;
    if !repl_project.exists() {
        return Err(anyhow!(
            "repl project missing at {}",
            repl_project.display()
        ));
    }
    let julia_src = "using ShipToolsRepl; ShipToolsRepl.serve(stdin, stdout)";

    // JULIA_LOAD_PATH uses ':' on Unix and ';' on Windows. ORDER MATTERS: the
    // workspace project (`@` = --project) comes FIRST so a dependency the
    // workspace shares with the shim (e.g. JSON3) resolves from the WORKSPACE's
    // version, not the shim's (Codex review, 2026-07-20 — proved JSON3 was
    // resolving from julia/repl under --project=core). The shim comes second so
    // `using ShipToolsRepl` still resolves. Trailing separator preserves the
    // default `["@", "@v#.#", "@stdlib"]` entries (stdlib etc.) via the empty
    // token.
    #[cfg(windows)]
    let sep = ";";
    #[cfg(not(windows))]
    let sep = ":";
    let load_path = format!("@{sep}{}{sep}", repl_project.display());

    let mut cmd = Command::new(&julia_bin);
    cmd.env("JULIA_LOAD_PATH", &load_path)
        // The workspace root, for the shim's relative-path fallback and for
        // user scripts (pty sessions already get it; REPL children didn't).
        .env("SOT_WORKSPACE_ROOT", user_project)
        // Child cwd = the activated project, NOT the daemon's cwd (which is
        // launch-context-dependent — observed `$HOME` after a script
        // restart). User code's own relative I/O (`include`, `open`) then
        // resolves where the user expects: the workspace/project root.
        .current_dir(user_project)
        .arg(format!("--project={}", user_project.display()))
        .arg("-e")
        .arg(julia_src)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut contained = sig.spawn(&mut cmd).with_context(|| {
        format!(
            "spawn {julia_bin} --project={} (JULIA_LOAD_PATH={load_path})",
            user_project.display()
        )
    })?;

    let stdin = contained.stdin.take().context("repl child stdin missing")?;
    let stdout = contained.stdout.take().context("repl child stdout missing")?;
    let stderr = contained.stderr.take().context("repl child stderr missing")?;

    let (submit_tx, submit_rx) = mpsc::channel::<Submission>(16);
    let (stop_tx, stop_rx) = oneshot::channel::<()>();

    let stderr_tail = spawn_stderr_tail(stderr);

    // Child exists: open a new spawn generation (state -> Starting, announce).
    let my_gen = lifecycle_begin_starting(&lifecycle, &frame_tx, &workspace_id);

    let join = tokio::spawn(supervisor_task(
        contained, stdin, stdout, submit_rx, stop_rx, frame_tx, workspace_id, stderr_tail, lifecycle, my_gen, sig,
    ));
    Ok(Supervisor {
        tx: submit_tx,
        stop: Some(stop_tx),
        join,
    })
}

/// One running REPL supervisor and its owner's three controls: the submission sender, the explicit stop and the
/// task's join handle, whose result is the checked retirement (the tree's termination was requested and the direct
/// child reaped).
pub(super) struct Supervisor {
    pub(super) tx: mpsc::Sender<Submission>,
    stop: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<std::result::Result<(), String>>,
}

impl Supervisor {
    /// Ask the task to close out, then wait up to `wait` for its checked retirement. A timeout leaves the task
    /// owned, so a later call waits for it again; any other result leaves nothing to wait for (`is_done`).
    pub(super) async fn retire(&mut self, wait: std::time::Duration) -> Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        match tokio::time::timeout(wait, &mut self.join).await {
            Err(_) => Err(anyhow!("repl retirement not finished within {wait:?}; it stays owned")),
            Ok(Err(e)) => Err(anyhow!("repl supervisor task failed: {e}")),
            Ok(Ok(Err(e))) => Err(anyhow!("repl retirement failed: {e}")),
            Ok(Ok(Ok(()))) => Ok(()),
        }
    }

    pub(super) fn is_done(&self) -> bool {
        self.join.is_finished()
    }
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

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines, reason = "the REPL supervisor task: one loop over the child's output and the submissions; predates the 100-line limit")]
async fn supervisor_task(
    mut contained: crate::lifecycle::child_signal::Contained,
    mut stdin: ChildStdin,
    stdout: tokio::process::ChildStdout,
    mut submit_rx: mpsc::Receiver<Submission>,
    mut stop_rx: oneshot::Receiver<()>,
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    workspace_id: Option<String>,
    stderr_tail: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    lifecycle: SharedLifecycle,
    my_gen: u64,
    sig: &'static crate::lifecycle::child_signal::Signal,
) -> std::result::Result<(), String> {
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
            // The daemon is shutting down: the signal has already killed the
            // child's tree.
            _ = sig.fired() => {
                break;
            }
            // The owner retires this supervisor (restart, or its handle is
            // gone): independent of the submit senders and of Julia reading
            // stdin.
            _ = &mut stop_rx => {
                break;
            }
            sub = submit_rx.recv() => {
                // Every sender is gone: the same closeout as a stop.
                let Some(sub) = sub else {
                    break;
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
                let mut line = match serde_json::to_string(&req) {
                    Ok(s) => s,
                    Err(e) => {
                        if let Some(reply) = sub.reply {
                            let _ = reply.send(Err(anyhow!("repl serialize: {e}")));
                        }
                        continue;
                    }
                };
                line.push('\n');
                // The write and flush give way to a stop or the signal, so a
                // child that reads nothing cannot hold the owner. The
                // submission stays ours until it is written; a cancelled one
                // is closed out with the rest and never resent.
                let written = tokio::select! {
                    biased;
                    _ = sig.fired() => Err("repl shutting down".to_string()),
                    _ = &mut stop_rx => Err("repl stopped".to_string()),
                    r = async {
                        stdin.write_all(line.as_bytes()).await?;
                        stdin.flush().await
                    } => r.map_err(|e| format!("repl stdin: {e}")),
                };
                if let Err(why) = written {
                    if let Some(reply) = sub.reply {
                        let _ = reply.send(Err(anyhow!(why)));
                    } else if let Some(eid) = sub.payload.get("eval_id").and_then(Value::as_u64) {
                        streaming.insert(eid);
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
                    Ok(Some(line)) => {
                        // First stdout line = the shim's serve loop is up:
                        // Starting -> Ready. Since the ADR 0009 ready
                        // sentinel (2026-08-24) that first line IS a designed
                        // `repl.ready` evt emitted as serve's first act, so a
                        // booted-but-idle child flips Ready at boot; for an
                        // older shim (no sentinel) the first eval output
                        // still triggers this. Gen-guarded + change-gated, so
                        // this is a one-shot per spawn and free thereafter.
                        lifecycle_transition(
                            &lifecycle,
                            my_gen,
                            ReplLifecycle::Ready,
                            &frame_tx,
                            &workspace_id,
                        );
                        route_line(
                            &line,
                            &mut pending,
                            &mut streaming,
                            &mut collectors,
                            &mut collector_ids,
                            &frame_tx,
                            &workspace_id,
                        )
                    }
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
            // The CHILD's exit ends this supervisor — not its pipes. Normally
            // the two coincide (the last stdout branch above sees EOF), but a
            // launcher that exits leaving a grandchild holding the inherited
            // pipes keeps both ends open forever: EOF never comes, so without
            // this branch the supervisor lives on with no child, holding a
            // submit channel whose sender still reports open. Every later
            // submit then queued into a channel nobody would ever read — a
            // wait with no child behind it and no respawn, which is the one
            // outcome a submit must never produce. `Contained::wait` is cancel
            // safe, so re-creating it each loop iteration is free, and this
            // branch is polled LAST (`biased`), so stdout the child already
            // wrote is still routed before we notice it is gone.
            status = contained.wait() => {
                match status {
                    Ok(s) => tracing::warn!(status = ?s, "repl child exited"),
                    Err(e) => tracing::warn!(error = %e, "repl child wait failed"),
                }
                break;
            }
        }
    }

    // Child death: flip to Dead FIRST (gen-guarded) so the announce precedes
    // the synthetic per-eval close-out frames below — a front-end reading in
    // order sees "the REPL died" before each "died mid-eval" error. An
    // applied transition also revokes this child's browser-served proxy
    // grants (a freed port must not stay dialable — it could be re-bound by
    // any other process); a stale (gen-rejected) death leaves the fresh
    // child's grants alone.
    if lifecycle_transition(
        &lifecycle,
        my_gen,
        ReplLifecycle::Dead,
        &frame_tx,
        &workspace_id,
    ) {
        crate::pages::proxy::revoke_browser_ports(browser_ports_key(&workspace_id));
    }
    for (_id, reply) in pending.drain() {
        let _ = reply.send(Err(anyhow!("repl terminated")));
    }
    // Accepted but unwritten submissions close out with the rest: no new one
    // is admitted, each reply fails, each streamed eval gets its close.
    submit_rx.close();
    while let Ok(sub) = submit_rx.try_recv() {
        if let Some(reply) = sub.reply {
            let _ = reply.send(Err(anyhow!("repl terminated")));
        } else if let Some(eid) = sub.payload.get("eval_id").and_then(Value::as_u64) {
            streaming.insert(eid);
        }
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
    // Retirement: request the tree's termination and check the direct
    // child's reap. The result is the owner's join value.
    drop(stdin);
    contained.kill().await.map(|_| ()).map_err(|e| e.to_string())
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
            tracing::warn!(error = %sot_protocol::codec::unparsed(&e, line.len()), "repl line parse failed");
            return;
        }
    };

    if env.op == "repl.ready" {
        // The boot sentinel (serve's first act). The Starting -> Ready flip
        // already fired on the first-line trigger in the caller; the envelope
        // itself carries no run data, so it is acknowledged and dropped here
        // rather than falling through to the res-ack path below.
        tracing::debug!(payload = %env.payload, "repl ready sentinel");
        return;
    }

    if env.op == "repl.frame" {
        // Streamed frame evt. Payload shape is `{eval_id, frame}`; pull both
        // out and fan the frame onto the broadcast bus. A missing eval_id is
        // skipped (malformed) rather than defaulted, so the frontend never
        // mis-attributes a frame.
        let eval_id = match env.payload.get("eval_id").and_then(Value::as_u64) {
            Some(id) => id,
            None => {
                // Its length only: the frame may carry a page's secret (ADR 0049, User isolation).
                tracing::warn!(len = line.len(), "repl.frame evt missing eval_id; dropping");
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
        // A `browser` frame (ADR 0032 BrowserView — wglshow / user-served
        // dashboard) announces a live loopback server in this child. Record
        // its port for the ADR-0035 proxy allowlist HERE, as the frame passes
        // through on its way to the FE — so the port is authorized strictly
        // before any browser can dial it, even when the child fell back to an
        // ephemeral port (or an old shim serves a fixed one). Loopback-only
        // by construction; revoked when this child dies or is respawned.
        if frame.get("kind").and_then(Value::as_str) == Some("browser") {
            if let Some(port) = frame
                .get("url")
                .and_then(Value::as_str)
                .and_then(sot_protocol::page_url::loopback_port_from_url)
            {
                crate::pages::proxy::record_browser_port(browser_ports_key(&workspace_id), port);
            }
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

#[cfg(test)]
mod interrupt_guard_tests {
    use super::*;

    /// `request_if_running` must be a no-spawn path: with no supervisor ever
    /// started it reports `Ok(None)` and leaves the lifecycle at `NotStarted`
    /// — the daemon-side twin of the #96 CLI guard, where `request`'s
    /// `ensure_supervisor` would boot a kernel (minutes of precompile) just
    /// to answer `interrupted:false`.
    #[tokio::test]
    async fn request_if_running_never_spawns() {
        let (tx, _rx) = broadcast::channel(8);
        let repl = Repl::new(tx, None, std::env::temp_dir(), crate::lifecycle::child_signal::process());
        let res = repl
            .request_if_running("repl.interrupt", serde_json::json!({}))
            .await
            .expect("no-child path must not error");
        assert!(res.is_none());
        assert_eq!(repl.state(), ReplLifecycle::NotStarted);
    }

    /// The boot sentinel is acknowledged and dropped by `route_line` — it
    /// must not be misread as a res ack (its id is 0) nor fanned onto the
    /// frame bus.
    #[test]
    fn route_line_drops_ready_sentinel() {
        let (frame_tx, mut frame_rx) = broadcast::channel(8);
        let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
        let (reply_tx, mut reply_rx) = oneshot::channel();
        pending.insert(0u64, reply_tx); // adversarial: a pending entry at the sentinel's id
        let mut streaming = std::collections::HashSet::new();
        let mut collectors = HashMap::new();
        let mut collector_ids = HashMap::new();
        route_line(
            r#"{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{"julia":"1.12.5","protocol":1}}"#,
            &mut pending,
            &mut streaming,
            &mut collectors,
            &mut collector_ids,
            &frame_tx,
            &None,
        );
        assert!(
            pending.contains_key(&0),
            "sentinel must not consume a pending reply"
        );
        assert!(reply_rx.try_recv().is_err());
        assert!(
            frame_rx.try_recv().is_err(),
            "sentinel is not a repl.frame; nothing goes on the bus"
        );
    }

    /// ADR 0049, User isolation: a REPL line that does not parse, or a frame with no eval id, is logged by its length
    /// and where it failed, never by its bytes: an announcement cut off inside a `wglshow` token leaves the token's
    /// first characters in the next line the reader sees, which no 32-character mask can recognise.
    #[test]
    fn a_line_that_does_not_parse_is_logged_without_its_bytes() {
        let logged = crate::sidecars::logged_by(|| {
            let (frame_tx, _frame_rx) = broadcast::channel(8);
            let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
            let mut streaming = std::collections::HashSet::new();
            let mut collectors = HashMap::new();
            let mut collector_ids = HashMap::new();
            for line in [
                // An announcement cut off inside the token, with the next envelope appended.
                r#"{"v":1,"id":0,"kind":"evt","op":"repl.frame","payload":{"eval_id":3,"frame":{"kind":"browser","url":"http://127.0.0.1:41234/0123456789ab{"v":1,"id":0,"kind":"evt","op":"repl.frame","payload":{}}"#,
                // A well-formed frame with no eval id.
                r#"{"v":1,"id":0,"kind":"evt","op":"repl.frame","payload":{"frame":{"kind":"browser","url":"http://127.0.0.1:41234/0123456789abcdef0123456789abcdef"}}}"#,
            ] {
                route_line(line, &mut pending, &mut streaming, &mut collectors, &mut collector_ids, &frame_tx, &None);
            }
        });
        assert_eq!(logged.lines().filter(|l| l.contains("WARN")).count(), 2, "{logged}");
        assert!(!logged.contains("0123"), "{logged}");
    }
}

#[cfg(test)]
mod wire_request_tests {
    use super::*;

    /// The request line the REPL child reads: fields in declaration order.
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
}
