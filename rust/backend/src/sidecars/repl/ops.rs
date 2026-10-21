//! The REPL ops: `repl.eval`, `repl.run_file`, `repl.interrupt` on a row's REPL child.

use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use crate::session::Session;
use crate::rows::row_or_reply;
use crate::rows::Workspaces;
use crate::server::reply::HandlerOutput;
use super::numbers::CallerNumbers;

/// Numbers a run and announces it to every window (ADR 0033, the one-numberer amendment): the daemon's number goes in
/// `payload`, a `started` frame labelled `origin` (a window's own run: "window"; else "session") goes on the bus, and
/// the caller's own number, if it gave one, stays the run's number on each connection of the caller's window
/// (`CallerNumbers`).
/// Returns the run's number and what `close_exec_run` needs to close it.
fn announce_run(
    numbers: &CallerNumbers,
    payload: &mut serde_json::Value,
    ws: &crate::rows::Workspace,
    workspaces: &Workspaces,
    display: String,
) -> (u64, String, tokio::sync::broadcast::Sender<super::ReplFrameMsg>) {
    let own = payload.get("eval_id").and_then(|v| v.as_u64());
    let number = numbers.number(own);
    let origin = match payload.get("origin").and_then(|v| v.as_str()) {
        Some(origin) => origin.to_string(),
        None if own.is_some() => "window".to_string(),
        None => "session".to_string(),
    };
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("eval_id".to_string(), json!(number));
    }
    let (frame_ws, frame_tx) =
        super::execute::announce_exec_started(Some(&origin), ws, workspaces, number, &format!("exec-{number}"), display);
    (number, frame_ws, frame_tx)
}

pub async fn handle_repl_eval(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
    numbers: &CallerNumbers,
) -> Result<HandlerOutput> {
    let workspace_id = payload_json
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let eval_id = payload_json.get("eval_id").and_then(|v| v.as_u64());
    tracing::info!(
        workspace_id = workspace_id.as_deref().unwrap_or("<default>"),
        eval_id,
        "repl.eval"
    );
    let ws = match row_or_reply(workspaces, workspace_id.as_deref(), req_id, op::REPL_EVAL) {
        Ok(ws) => ws,
        Err(reply) => return Ok(reply),
    };
    let repl = ws.repl(workspaces.repl_frame_tx());
    let mut forwarded = payload_json.clone();
    let display = super::execute::code_display(payload_json.get("code").and_then(|v| v.as_str()).unwrap_or(""));
    let (number, frame_ws, frame_tx) = announce_run(numbers, &mut forwarded, &ws, workspaces, display);
    // Fire-and-forget: queue the eval at the supervisor and return an ack
    // immediately. The eval's frames (stdout/value/error/done) stream as
    // separate `repl.frame` evts over the broadcast bus; the frontend keys
    // completion off the terminal `done` frame, not this ack. Returning here
    // (instead of awaiting the eval) keeps the connection loop free to read a
    // mid-eval `repl.interrupt`.
    if let Err(e) = repl.submit(op::REPL_EVAL, forwarded, numbers).await {
        let error = format!("{e:#}");
        numbers.refused(number);
        super::execute::close_exec_run(&frame_tx, &frame_ws, number, 0, Some(&error));
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_EVAL,
                json!({
                    "error": error,
                    "code": submit_code(&e, "repl_eval_failed"),
                }),
            ),
            None,
        )]);
    }
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(
            req_id,
            op::REPL_EVAL,
            // Full ReplEvalRes shape (frames:[] + elapsed_ms) so the FE
            // deserializes cleanly and its empty-frames guard fires (content
            // already streamed via repl.frame evts). Real elapsed_ms rides the
            // done frame.
            json!({ "eval_id": eval_id.unwrap_or(number), "elapsed_ms": 0, "frames": [], "accepted": true }),
        )
        .with_rev(rev),
        None,
    )])
}

#[allow(clippy::too_many_lines, reason = "the repl.run_file handler: resolves the file and project, runs it and replies; predates the 100-line limit")]
pub async fn handle_repl_run_file(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
    numbers: &CallerNumbers,
) -> Result<HandlerOutput> {
    let workspace_id = payload_json
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let fresh = payload_json
        .get("fresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let path_str = payload_json
        .get("path")
        .and_then(|v| v.as_str())
        .map(String::from);
    let eval_id = payload_json.get("eval_id").and_then(|v| v.as_u64());
    tracing::info!(
        workspace_id = workspace_id.as_deref().unwrap_or("<default>"),
        fresh,
        eval_id,
        path = path_str.as_deref().unwrap_or(""),
        "repl.run_file"
    );
    let ws = match row_or_reply(workspaces, workspace_id.as_deref(), req_id, op::REPL_RUN_FILE) {
        Ok(ws) => ws,
        Err(reply) => return Ok(reply),
    };
    let repl = ws.repl(workspaces.repl_frame_tx());

    // A `fresh` run restarts the REPL into the workspace's project here, then forwards a plain include (`fresh=false`),
    // so the run's frames stream over the broadcast bus and this handler returns only the ack.
    let mut forwarded_payload = payload_json.clone();
    // Resolve a RELATIVE path against the WORKSPACE ROOT and forward it
    // ABSOLUTE. The Julia child resolves relative paths against its own cwd
    // — inherited from the daemon, whose cwd is launch-context-dependent
    // (observed `$HOME` after a script restart) — so a workspace-relative
    // path like `dev/output/x.jl` resolved to a nonexistent `$HOME/dev/…`
    // and the run died as a missing-file res that the fire-and-forget path
    // DROPS silently (2026-07-24 field report: "--fresh include never
    // runs"). `sot-fe repl run` documents its path as workspace-relative;
    // make the daemon honor that contract deterministically.
    let path_str = path_str.map(|p| {
        let pb = std::path::PathBuf::from(&p);
        if pb.is_absolute() {
            p
        } else {
            let abs = ws.project_root.join(pb).display().to_string();
            if let Some(obj) = forwarded_payload.as_object_mut() {
                obj.insert("path".to_string(), serde_json::Value::String(abs.clone()));
            }
            abs
        }
    });
    // Captured for the ack so the FE's fresh-`r` status line can show the
    // project the file was bounced into. Only resolved for fresh runs; a
    // fresh=false include leaves them None → FE degrades to "(no project)".
    let mut project_dir_str: Option<String> = None;
    let mut project_source_str: Option<String> = None;
    // A run with no path has nothing to run, and the shim answers it with a res alone, which this fire-and-forget op
    // drops: no window would ever see it end.
    let Some(ref p) = path_str else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_RUN_FILE,
                json!({
                    "error": "repl.run_file requires a path",
                    "code": "bad_request",
                }),
            ),
            None,
        )]);
    };
    if fresh {
        let abs_path = std::path::PathBuf::from(p);
        // The workspace REPL defaults to the WORKSPACE PACKAGE's env — the #44
        // per-package env-fix (decided 2026-07-22: the repl has this repo as its
        // default env, and defaults to each package's env). A --fresh restart
        // therefore activates SOT_WORKSPACE_ROOT's Project.toml, SAME as the default
        // (non-fresh) REPL — NOT a project
        // walked up/discovered from the file's path, which mis-picked a parent
        // (e.g. ~/dev when a relative path resolved there) and broke
        // `using <workspace package>`. Walk-up discovery is only the fallback
        // when the workspace root itself is not a package (no Project.toml).
        let (project_dir, project_source) = if ws.project_root.join("Project.toml").is_file() {
            (ws.project_root.clone(), "workspace")
        } else {
            closest_project_dir(&abs_path).unwrap_or_else(|| (ws.project_root.clone(), "fallback"))
        };
        project_dir_str = Some(project_dir.display().to_string());
        project_source_str = Some(project_source.to_string());
        if let Err(e) = repl.restart_with_project(&project_dir).await {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::REPL_RUN_FILE,
                    json!({
                        "error": format!("repl restart failed: {e:#}"),
                        "code": "repl_restart_failed",
                        "project_dir": project_dir.display().to_string(),
                        "project_source": project_source,
                    }),
                ),
                None,
            )]);
        }
        // Rewrite the forwarded payload to ask the Julia side for a plain
        // include — we've already done the env bounce here.
        if let Some(obj) = forwarded_payload.as_object_mut() {
            obj.insert("fresh".to_string(), serde_json::Value::Bool(false));
        }
    }

    // The run is announced after the restart and before the run, so every window's entry for it follows the
    // restart's lifecycle frames; the shim's `done`, or the close-out of a child that ends first, closes it.
    let name = std::path::Path::new(p).file_name().and_then(|s| s.to_str()).unwrap_or("?.jl");
    let display = if fresh { format!("run {name} (fresh)") } else { format!("run {name}") };
    let (number, frame_ws, frame_tx) = announce_run(numbers, &mut forwarded_payload, &ws, workspaces, display);

    // Fire-and-forget: queue the run and ack immediately. Frames stream as
    // `repl.frame` evts; the frontend keys completion off the `done` frame.
    if let Err(e) = repl.submit(op::REPL_RUN_FILE, forwarded_payload, numbers).await {
        let error = format!("{e:#}");
        numbers.refused(number);
        super::execute::close_exec_run(&frame_tx, &frame_ws, number, 0, Some(&error));
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_RUN_FILE,
                json!({
                    "error": error,
                    "code": submit_code(&e, "repl_run_file_failed"),
                }),
            ),
            None,
        )]);
    }
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(
            req_id,
            op::REPL_RUN_FILE,
            // Full ReplRunFileRes shape (frames:[] + required fields) so the
            // FE deserializes it cleanly and its empty-frames guard fires
            // (content already streamed via repl.frame evts). Real elapsed_ms
            // rides the done frame; project_dir/source drive the fresh-`r`
            // status line.
            json!({
                "eval_id": eval_id.unwrap_or(number),
                "path": path_str.clone().unwrap_or_default(),
                "fresh": fresh,
                "elapsed_ms": 0,
                "project_dir": project_dir_str,
                "project_source": project_source_str,
                "frames": [],
                "accepted": true
            }),
        )
        .with_rev(rev),
        None,
    )])
}

/// The code of a submission's error: `repl_busy` when the REPL's queue was full, else the op's own.
fn submit_code(e: &anyhow::Error, failed: &'static str) -> &'static str {
    if e.is::<crate::sidecars::repl::ReplBusy>() {
        "repl_busy"
    } else {
        failed
    }
}

/// Walk up from `path`'s parent looking for the nearest `Project.toml`.
/// Returns `(dir, "discovered")` if found, `None` to let the caller
/// fall back.
fn closest_project_dir(path: &std::path::Path) -> Option<(std::path::PathBuf, &'static str)> {
    let mut dir = path.parent()?.to_path_buf();
    loop {
        if dir.join("Project.toml").is_file() {
            return Some((dir, "discovered"));
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub async fn handle_repl_interrupt(
    req_id: u64,
    mut payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
    numbers: &CallerNumbers,
) -> Result<HandlerOutput> {
    let workspace_id = payload_json
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    tracing::info!(
        workspace_id = workspace_id.as_deref().unwrap_or("<default>"),
        "repl.interrupt"
    );
    let ws = match row_or_reply(workspaces, workspace_id.as_deref(), req_id, op::REPL_INTERRUPT) {
        Ok(ws) => ws,
        Err(reply) => return Ok(reply),
    };
    let repl = ws.repl(workspaces.repl_frame_tx());
    // The evals the caller names in its window's numbers reach the REPL as the daemon's (`CallerNumbers`).
    if let Some(named) = payload_json.get("eval_ids").and_then(|v| v.as_array()) {
        let named: Vec<u64> = named.iter().filter_map(|v| v.as_u64()).collect();
        payload_json["eval_ids"] = json!(numbers.daemon_numbers(&named));
    }
    // Daemon-side interrupt guard (P0.5 pre-work; twin of the #96 CLI
    // pre-flight, now enforced for EVERY caller — FE Ctrl-C and raw-socket
    // included): an interrupt must never be the thing that spawns a kernel.
    // Only a `ready` child is forwarded to; `starting` is refused too (the
    // serve loop isn't consuming yet — boot ≠ wedge, and a queued interrupt
    // would land on the first legitimate eval instead). `request_if_running`
    // closes the remaining race: even a stale `ready` reading cannot respawn.
    let state = repl.state();
    let result = match state {
        crate::sidecars::repl::ReplLifecycle::Ready => {
            repl.request_if_running("repl.interrupt", payload_json).await
        }
        _ => Ok(None),
    };
    let (_, rev) = session.snapshot().await;
    let payload = match result {
        Ok(Some(v)) => v,
        Ok(None) => {
            // `ready` reaching here means the state read raced a child death
            // (request_if_running found the sender closed) — name that fact
            // so the note isn't self-contradictory ("no child" + "ready").
            let note = if state == crate::sidecars::repl::ReplLifecycle::Ready {
                "no running repl child (repl_state=ready but supervisor sender closed — child just exited)".to_string()
            } else {
                format!("no running repl child (repl_state={})", state.as_str())
            };
            json!({ "interrupted": false, "note": note })
        }
        Err(e) => json!({
            "error": format!("{e:#}"),
            "code": submit_code(&e, "repl_interrupt_failed"),
        }),
    };
    Ok(vec![(
        Frame::res(req_id, op::REPL_INTERRUPT, payload).with_rev(rev),
        None,
    )])
}

/// The daemon forwards an interrupt's payload to the REPL child with the evals it names (`eval_ids`) in the daemon's
/// numbers, so they reach the shim, which decides which eval it means.
#[cfg(all(test, unix))]
#[path = "ops_interrupt_tests.rs"]
mod interrupt_tests;

/// No REPL submission waits on a child that reads nothing: an interrupt's answer is waited for at most 10 s
/// (`REPL_INTERRUPT_WAIT`), and once the REPL's queue is full an eval, an interrupt and an execute are each refused at
/// once (`repl_busy`, and `repl.execute`'s outcome `busy`).
#[cfg(all(test, unix))]
#[path = "ops_busy_tests.rs"]
mod busy_tests;

/// `sot-fe repl run --fresh` numbers no run of its own. Every window learns of the run from the `started` frame the
/// daemon sends once the REPL has restarted, under a number from the daemon's own space, and the run's output and its
/// `done` come under that number: a window's drawer makes an entry only for its own evals and for a `started` frame
/// (`on_repl_frame_streamed`). A run that was waiting when the restart came still gets its answer, and every window's
/// entry for it is closed with the reason: a restart says it restarted, a death that it died.
#[cfg(all(test, unix))]
#[path = "ops_fresh_tests.rs"]
mod fresh_tests;
