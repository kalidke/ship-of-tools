//! The REPL child's task: spawning it, its wire to the child, routing its lines and closing out on death.

use std::collections::HashMap;
use std::process::Stdio;

use anyhow::Context;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

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

pub(super) fn spawn_supervisor(
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    workspace_id: Option<String>,
    lifecycle: SharedLifecycle,
) -> Result<mpsc::Sender<Submission>> {
    let repl_project = Repl::repl_project();
    let julia_bin = crate::sidecars::julia::resolve_bin_or_bare();
    if !repl_project.exists() {
        return Err(anyhow!(
            "repl project missing at {}",
            repl_project.display()
        ));
    }
    let julia_src = "using ShipToolsRepl; ShipToolsRepl.serve(stdin, stdout)";

    let mut child: Child = Command::new(&julia_bin)
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

    // Child exists: open a new spawn generation (state -> Starting, announce).
    // Deliberately after `.spawn()` succeeds — a failed spawn leaves the prior
    // state (NotStarted/Dead) intact, which is the truthful reading.
    let my_gen = lifecycle_begin_starting(&lifecycle, &frame_tx, &workspace_id);

    tokio::spawn(supervisor_task(
        child, stdin, stdout, submit_rx, frame_tx, workspace_id, stderr_tail, lifecycle, my_gen,
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
/// trick is `JULIA_LOAD_PATH=@:<repl_project>:` — the workspace project (`@` =
/// `--project`) FIRST so a dependency the workspace shares with the shim (e.g.
/// JSON3) resolves from the WORKSPACE's version, then the REPL project (where
/// `ShipToolsRepl` lives) as a fallback for the shim's own deps, then the
/// default load path (stdlib, etc.) via the trailing colon so `using` of
/// standard packages still works inside the user code.
pub(super) fn spawn_supervisor_with_project(
    user_project: &Path,
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    workspace_id: Option<String>,
    lifecycle: SharedLifecycle,
) -> Result<mpsc::Sender<Submission>> {
    let repl_project = Repl::repl_project();
    let julia_bin = crate::sidecars::julia::resolve_bin_or_bare();
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

    let mut child: Child = Command::new(&julia_bin)
        .env("JULIA_LOAD_PATH", &load_path)
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

    // Child exists: open a new spawn generation (state -> Starting, announce).
    let my_gen = lifecycle_begin_starting(&lifecycle, &frame_tx, &workspace_id);

    tokio::spawn(supervisor_task(
        child, stdin, stdout, submit_rx, frame_tx, workspace_id, stderr_tail, lifecycle, my_gen,
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

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines, reason = "the REPL supervisor task: one loop over the child's output and the submissions; predates the 100-line limit")]
async fn supervisor_task(
    mut child: Child,
    mut stdin: ChildStdin,
    stdout: tokio::process::ChildStdout,
    mut submit_rx: mpsc::Receiver<Submission>,
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    workspace_id: Option<String>,
    stderr_tail: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    lifecycle: SharedLifecycle,
    my_gen: u64,
) {
    let _child_guard = crate::lifecycle::child_signal::ChildGuard::new();
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
            // The daemon is shutting down: nothing kills this child at
            // `process::exit`, so it is killed here.
            _ = crate::lifecycle::child_signal::fired() => {
                let _ = child.kill().await;
                break;
            }
            sub = submit_rx.recv() => {
                let Some(sub) = sub else {
                    drop(stdin);
                    let _ = child.wait().await;
                    // Intentional teardown (sender dropped — restart or
                    // shutdown). Gen-guarded: when a restart has already
                    // opened the next generation this is a no-op, so the
                    // fresh child's `Starting` isn't stomped to `Dead` — and
                    // its proxy grants aren't revoked out from under it
                    // (an applied Dead means WE were the current child, so
                    // our browser-served ports die with us).
                    if lifecycle_transition(
                        &lifecycle,
                        my_gen,
                        ReplLifecycle::Dead,
                        &frame_tx,
                        &workspace_id,
                    ) {
                        crate::pages::proxy::revoke_browser_ports(browser_ports_key(&workspace_id));
                    }
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
            // outcome a submit must never produce. `Child::wait` is cancel
            // safe, so re-creating it each loop iteration is free, and this
            // branch is polled LAST (`biased`), so stdout the child already
            // wrote is still routed before we notice it is gone.
            status = child.wait() => {
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
        let repl = Repl::new(tx, None, None);
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
