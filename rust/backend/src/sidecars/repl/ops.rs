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
        // per-package env-fix, and the owner directive (2026-07-22): "the repl
        // should have this repo as its default env … defaults to each package's
        // env." A --fresh restart therefore activates SOT_WORKSPACE_ROOT's
        // Project.toml, SAME as the default (non-fresh) REPL — NOT a project
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
mod interrupt_tests {
    use super::*;
    use crate::sidecars::contract_tests::{pin_env, within};
    use std::time::Duration;

    /// A `julia` stand-in that says it is ready and answers each interrupt with a note holding a byte that is not UTF-8.
    const STUB_BYTE: &str = r#"#!/bin/sh
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
while read -r line; do
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  case "$line" in
    *repl.interrupt*) printf '{"v":1,"id":%s,"kind":"res","op":"repl.interrupt","payload":{"interrupted":true,"note":"a\377b"}}\n' "$id" ;;
  esac
done
"#;

    /// A REPL child's line may hold any byte: its answer arrives with U+FFFD for the byte, and the same child answers
    /// the next request.
    #[tokio::test]
    async fn a_repl_line_holding_an_invalid_byte_keeps_the_child() {
        // Removed when dropped, after a failed assertion too.
        let tmp = tempfile::Builder::new().prefix("sot-repl-byte-").tempdir().unwrap();
        let dir = tmp.path();
        let resources = dir.join("resources");
        std::fs::create_dir_all(resources.join("julia").join("repl")).unwrap();
        let root = dir.join("row");
        std::fs::create_dir_all(&root).unwrap();
        let julia = dir.join("julia");
        sot_log::test_exec::write_executable(&julia, STUB_BYTE);
        let _pin = pin_env(&julia, &resources);

        let (frame_tx, _bus) = tokio::sync::broadcast::channel(256);
        let workspaces = Workspaces::new();
        workspaces.set_repl_frame_tx(frame_tx);
        let session = Session::new();
        let ws = crate::rows::Workspace::from_label("byterow", root, false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        workspaces.insert(ws);
        let row = row_or_reply(&workspaces, Some(&id), 0, op::REPL_EVAL).unwrap_or_else(|_| panic!("setup: the row"));
        handle_repl_eval(1, json!({ "workspace_id": id, "eval_id": 7, "code": "1" }), &session, &workspaces, &Default::default())
            .await
            .expect("setup: the eval starts the child");
        within(Duration::from_secs(30), "the child is ready", || row.repl_state() == "ready").await;

        let mut answers = Vec::new();
        for req in 2..4 {
            let out = handle_repl_interrupt(req, json!({ "workspace_id": id, "eval_ids": [7] }), &session, &workspaces, &Default::default())
                .await
                .expect("handler");
            answers.push(out[0].0.payload.clone());
        }
        let answered = json!({ "interrupted": true, "note": "a\u{fffd}b" });
        assert_eq!(answers, [answered.clone(), answered]);
    }

    /// A `julia` stand-in: it says it is ready, then logs each request line beside itself and answers it.
    const STUB: &str = r#"#!/bin/sh
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
while read -r line; do
  printf '%s\n' "$line" >> "$0.log"
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  printf '{"v":1,"id":%s,"kind":"res","op":"repl.interrupt","payload":{"interrupted":true}}\n' "$id"
done
"#;

    #[tokio::test]
    async fn an_interrupt_reaches_the_child_with_the_evals_it_names() {
        // Removed when dropped, after a failed assertion too.
        let tmp = tempfile::Builder::new().prefix("sot-repl-interrupt-").tempdir().unwrap();
        let dir = tmp.path();
        let resources = dir.join("resources");
        std::fs::create_dir_all(resources.join("julia").join("repl")).unwrap();
        let root = dir.join("row");
        std::fs::create_dir_all(&root).unwrap();
        let julia = dir.join("julia");
        sot_log::test_exec::write_executable(&julia, STUB);
        let _pin = pin_env(&julia, &resources);

        let (frame_tx, _bus) = tokio::sync::broadcast::channel(256);
        let workspaces = Workspaces::new();
        workspaces.set_repl_frame_tx(frame_tx);
        let session = Session::new();
        let ws = crate::rows::Workspace::from_label("introw", root, false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        workspaces.insert(ws);
        let row = row_or_reply(&workspaces, Some(&id), 0, op::REPL_EVAL).unwrap_or_else(|_| panic!("setup: the row"));
        // A window's connection, as the roster makes one; the window redials before its Ctrl+C.
        let clients = crate::clients::Clients::new();
        let window = || clients.register("c", "0.6.6", 1, "fe".into(), Some("h".into()), Some("w".into()), None);
        let conn = window();
        handle_repl_eval(1, json!({ "workspace_id": id, "eval_id": 205, "code": "1" }), &session, &workspaces, &conn.numbers())
            .await
            .expect("setup: the eval starts the child");
        within(Duration::from_secs(30), "the child is ready", || row.repl_state() == "ready").await;
        drop(conn);
        let again = window();

        // The eval is named by the window's own number (205) and an announced run by the daemon's.
        let named = json!({ "workspace_id": id, "eval_ids": [205, 1u64 << 40] });
        let out = handle_repl_interrupt(2, named, &session, &workspaces, &again.numbers()).await.expect("handler");
        assert_eq!(out[0].0.payload, json!({ "interrupted": true }), "the child's answer is the reply");
        let logged: Vec<serde_json::Value> = std::fs::read_to_string(dir.join("julia.log"))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let eval = logged.iter().find(|v| v["op"] == op::REPL_EVAL).expect("the eval reached the child");
        let number = eval["payload"]["eval_id"].as_u64().expect("the eval's number");
        assert!(number >= 1 << 40, "the child runs the eval under the daemon's number: {eval}");
        let interrupts: Vec<&serde_json::Value> = logged.iter().filter(|v| v["op"] == op::REPL_INTERRUPT).collect();
        assert_eq!(interrupts.len(), 1, "one interrupt reached the child: {logged:?}");
        let reached = json!({ "workspace_id": id, "eval_ids": [number, 1u64 << 40] });
        assert_eq!(interrupts[0]["payload"], reached, "the child is named the eval by the daemon's number");
    }
}

/// No REPL submission waits on a child that reads nothing: an interrupt's answer is waited for at most 10 s
/// (`REPL_INTERRUPT_WAIT`), and once the REPL's queue is full an eval, an interrupt and an execute are each refused at
/// once (`repl_busy`, and `repl.execute`'s outcome `busy`).
#[cfg(all(test, unix))]
mod busy_tests {
    use super::*;
    use crate::sidecars::contract_tests::{pin_env, within, EnvPin};
    use std::time::Duration;

    /// A `julia` stand-in: it says it is ready, then never reads its input, and goes on through the interrupt signal.
    const DEAF: &str = "#!/bin/sh\ntrap '' INT\nprintf '{\"v\":1,\"id\":0,\"kind\":\"evt\",\"op\":\"repl.ready\",\"payload\":{}}\\n'\nexec sleep 60\n";

    /// `fut`'s output, failing the test when it waits 15 s.
    async fn bounded<F: std::future::Future>(what: &str, fut: F) -> F::Output {
        tokio::time::timeout(Duration::from_secs(15), fut)
            .await
            .unwrap_or_else(|_| panic!("{what} waited 15 s on a REPL that reads nothing"))
    }

    /// The reply's `code`, or `accepted` for an eval's ack.
    fn code_of(out: &HandlerOutput) -> String {
        out[0].0.payload.get("code").and_then(|c| c.as_str()).unwrap_or("accepted").to_string()
    }

    /// A row whose REPL child is the deaf stand-in, started by one eval and ready. Fields drop in order: the row and
    /// its child first, then the environment pin, then the folder.
    struct Deaf {
        session: Session,
        workspaces: Workspaces,
        id: String,
        _pin: EnvPin,
        _tmp: tempfile::TempDir,
    }

    async fn deaf_row() -> Deaf {
        row_running(DEAF).await
    }

    async fn row_running(stub: &str) -> Deaf {
        let tmp = tempfile::Builder::new().prefix("sot-repl-busy-").tempdir().unwrap();
        let dir = tmp.path();
        let resources = dir.join("resources");
        std::fs::create_dir_all(resources.join("julia").join("repl")).unwrap();
        let root = dir.join("row");
        std::fs::create_dir_all(&root).unwrap();
        let julia = dir.join("julia");
        sot_log::test_exec::write_executable(&julia, stub);
        let pin = pin_env(&julia, &resources);
        let (frame_tx, _bus) = tokio::sync::broadcast::channel(256);
        let workspaces = Workspaces::new();
        workspaces.set_repl_frame_tx(frame_tx);
        let session = Session::new();
        let ws = crate::rows::Workspace::from_label("busyrow", root, false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        workspaces.insert(ws);
        let row = row_or_reply(&workspaces, Some(&id), 0, op::REPL_EVAL).unwrap_or_else(|_| panic!("setup: the row"));
        let first = json!({ "workspace_id": id, "eval_id": 0, "code": "1" });
        let started = bounded("eval 0", handle_repl_eval(0, first, &session, &workspaces, &Default::default())).await;
        assert_eq!(code_of(&started.expect("handler")), "accepted");
        within(Duration::from_secs(30), "the child is ready", || row.repl_state() == "ready").await;
        Deaf { session, workspaces, id, _pin: pin, _tmp: tmp }
    }

    #[tokio::test]
    async fn an_unanswered_interrupt_ends_at_its_bound() {
        let row = deaf_row().await;
        let t0 = std::time::Instant::now();
        let conn = CallerNumbers::default();
        let interrupt = handle_repl_interrupt(1, json!({ "workspace_id": row.id }), &row.session, &row.workspaces, &conn);
        let unanswered = bounded("an interrupt", interrupt).await.expect("handler");
        let waited = t0.elapsed();
        assert_eq!(code_of(&unanswered), "repl_interrupt_failed");
        let error = unanswered[0].0.payload["error"].as_str().unwrap_or("").to_string();
        assert!(error.contains("--fresh"), "the error names the restart that ends a non-yielding eval: {error}");
        let bound = Duration::from_secs(10);
        assert!(waited >= bound && waited < bound + Duration::from_secs(5), "{waited:?}");
        println!("repl-busy: an unanswered interrupt -> repl_interrupt_failed after {waited:?}");
    }

    /// Stand-ins that record each interrupt signal they get in `julia.signals` and otherwise read nothing after their
    /// first request (the eval 0 `deaf_row` sends). `ANSWERED_FIRST` ends that eval at once (an idle REPL, a marker
    /// `julia.done` after); `ENDS_AFTER_ASK` reads the interrupt too and ends the eval 0.5 s later.
    const ANSWERED_FIRST: &str = r#"#!/bin/sh
trap 'echo x >> "$0.signals"' INT
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
IFS= read -r line
eid=$(printf '%s' "$line" | sed -n -e 's/.*"eval_id":\([0-9]*\).*/\1/p')
printf '{"v":1,"id":1,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"done","eval_id":%s,"elapsed_ms":1}}}\n' "$eid" "$eid"
: > "$0.done"
while :; do sleep 0.1; done
"#;
    const ENDS_AFTER_ASK: &str = r#"#!/bin/sh
trap 'echo x >> "$0.signals"' INT
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
IFS= read -r line
eid=$(printf '%s' "$line" | sed -n -e 's/.*"eval_id":\([0-9]*\).*/\1/p')
IFS= read -r line
sleep 0.5
printf '{"v":1,"id":1,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"done","eval_id":%s,"elapsed_ms":1}}}\n' "$eid" "$eid"
: > "$0.done"
while :; do sleep 0.1; done
"#;

    /// An interrupt that goes unanswered for its whole bound, and the signals the child got meanwhile.
    async fn unanswered_interrupt_signals(row: &Deaf) -> String {
        let signals = row._tmp.path().join("julia.signals");
        let conn = CallerNumbers::default();
        let interrupt = handle_repl_interrupt(1, json!({ "workspace_id": row.id }), &row.session, &row.workspaces, &conn);
        let unanswered = bounded("an interrupt", interrupt).await.expect("handler");
        assert_eq!(code_of(&unanswered), "repl_interrupt_failed");
        std::fs::read_to_string(signals).unwrap_or_default()
    }

    /// A REPL with no eval in flight is never sent the interrupt signal, however long its interrupt goes unanswered:
    /// a signal that lands in an idle REPL can end it and its state with it.
    #[tokio::test]
    async fn an_idle_repl_is_never_signalled() {
        let row = row_running(ANSWERED_FIRST).await;
        let done = row._tmp.path().join("julia.done");
        within(Duration::from_secs(10), "the eval ended", || done.exists()).await;
        assert_eq!(unanswered_interrupt_signals(&row).await, "", "an idle REPL got a signal");
        let live = row_or_reply(&row.workspaces, Some(&row.id), 0, op::REPL_EVAL).unwrap_or_else(|_| panic!("the row"));
        assert_eq!(live.repl_state(), "ready", "the idle REPL is still running");
    }

    /// An eval whose `done` arrives before the interrupt's wait ends ends the escalation: no signal follows it.
    #[tokio::test]
    async fn no_signal_follows_the_evals_done() {
        let row = row_running(ENDS_AFTER_ASK).await;
        assert_eq!(unanswered_interrupt_signals(&row).await, "", "a signal followed the eval's done");
    }

    #[tokio::test]
    async fn a_full_queue_is_refused_at_once() {
        let row = deaf_row().await;
        let (session, workspaces, id) = (&row.session, &row.workspaces, &row.id);
        let code = "x".repeat(64 * 1024);
        let mut codes = Vec::new();
        for n in 1..=20u64 {
            let eval = json!({ "workspace_id": id, "eval_id": n, "code": code });
            let out = bounded(&format!("eval {n}"), handle_repl_eval(n, eval, session, workspaces, &Default::default())).await;
            codes.push(code_of(&out.expect("handler")));
        }
        let busy = codes.iter().filter(|c| *c == "repl_busy").count();
        assert!(busy >= 1 && codes.iter().all(|c| c == "accepted" || c == "repl_busy"), "{codes:?}");
        let conn = CallerNumbers::default();
        let interrupt = handle_repl_interrupt(21, json!({ "workspace_id": id }), session, workspaces, &conn);
        let refused = bounded("an interrupt", interrupt).await;
        assert_eq!(code_of(&refused.expect("handler")), "repl_busy");
        let execute = json!({ "workspace_id": id, "input": { "kind": "eval", "code": "1" } });
        let execute = crate::sidecars::repl::execute::handle_repl_execute(22, execute, session, workspaces);
        let executed = bounded("an execute", execute).await;
        assert_eq!(executed.expect("handler")[0].0.payload["outcome"], "busy");
        println!(
            "repl-busy: 20 evals to a full queue -> {} accepted, {busy} repl_busy; then interrupt -> repl_busy, execute -> busy",
            20 - busy
        );
    }
}

/// `sot-fe repl run --fresh` numbers no run of its own. Every window learns of the run from the `started` frame the
/// daemon sends once the REPL has restarted, under a number from the daemon's own space, and the run's output and its
/// `done` come under that number: a window's drawer makes an entry only for its own evals and for a `started` frame
/// (`on_repl_frame_streamed`). A run that was waiting when the restart came still gets its answer, and every window's
/// entry for it is closed with the reason: a restart says it restarted, a death that it died.
#[cfg(all(test, unix))]
mod fresh_tests {
    use super::*;
    use crate::sidecars::contract_tests::{pin_env, within, EnvPin};
    use std::time::Duration;

    /// A `julia` stand-in: it says it is ready, then answers each request with one line of output, its `done` and its
    /// `res`, under the request's `eval_id` (0 when it has none).
    const ANSWERS: &str = r#"#!/bin/sh
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  eid=$(printf '%s' "$line" | sed -n -e 's/.*"eval_id":\([0-9]*\).*/\1/p')
  eid=${eid:-0}
  printf '{"v":1,"id":%s,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"stdout","text":"ran\\n"}}}\n' "$id" "$eid"
  printf '{"v":1,"id":%s,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"done","eval_id":%s,"elapsed_ms":1}}}\n' "$id" "$eid" "$eid"
  printf '{"v":1,"id":%s,"kind":"res","op":"repl.run_file","payload":{}}\n' "$id"
done
"#;

    /// A `julia` stand-in that says it is ready and then reads nothing, as a REPL busy in a loop that never yields.
    const BUSY: &str = "#!/bin/sh\nprintf '{\"v\":1,\"id\":0,\"kind\":\"evt\",\"op\":\"repl.ready\",\"payload\":{}}\\n'\nexec sleep 60\n";

    /// `BUSY`, after a line on stderr.
    const BUSY_NOISY: &str = "#!/bin/sh\necho 'repl-restart-red' >&2\nprintf '{\"v\":1,\"id\":0,\"kind\":\"evt\",\"op\":\"repl.ready\",\"payload\":{}}\\n'\nexec sleep 60\n";

    /// A `julia` stand-in that says it is ready, reads two requests and exits: it dies with both in flight.
    const DIES: &str = "#!/bin/sh\nprintf '{\"v\":1,\"id\":0,\"kind\":\"evt\",\"op\":\"repl.ready\",\"payload\":{}}\\n'\nread -r a\nread -r b\nexit 3\n";

    /// A row whose REPL child is `stub`, a script `fresh.jl` in it, and the frame bus every window reads. Fields drop
    /// in order: the row and its child first, then the environment pin, then the folder.
    struct Row {
        session: Session,
        workspaces: Workspaces,
        id: String,
        script: String,
        bus: tokio::sync::broadcast::Receiver<crate::sidecars::repl::ReplFrameMsg>,
        _pin: EnvPin,
        _tmp: tempfile::TempDir,
    }

    fn row(stub: &str) -> Row {
        let tmp = tempfile::Builder::new().prefix("sot-repl-fresh-").tempdir().unwrap();
        let dir = tmp.path();
        let resources = dir.join("resources");
        std::fs::create_dir_all(resources.join("julia").join("repl")).unwrap();
        let root = dir.join("row");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Project.toml"), "").unwrap();
        std::fs::write(root.join("fresh.jl"), "1\n").unwrap();
        let julia = dir.join("julia");
        sot_log::test_exec::write_executable(&julia, stub);
        let pin = pin_env(&julia, &resources);
        let (frame_tx, bus) = tokio::sync::broadcast::channel(256);
        let workspaces = Workspaces::new();
        workspaces.set_repl_frame_tx(frame_tx);
        let ws = crate::rows::Workspace::from_label("freshrow", root.clone(), false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        workspaces.insert(ws);
        let script = root.join("fresh.jl").to_string_lossy().into_owned();
        Row { session: Session::new(), workspaces, id, script, bus, _pin: pin, _tmp: tmp }
    }

    /// The next frame every window gets, as (eval id, frame), failing the test after 30 s.
    async fn next_frame(
        bus: &mut tokio::sync::broadcast::Receiver<crate::sidecars::repl::ReplFrameMsg>,
    ) -> (u64, serde_json::Value) {
        let msg = tokio::time::timeout(Duration::from_secs(30), bus.recv())
            .await
            .expect("a frame within 30 s")
            .expect("the frame bus");
        (msg.eval_id, msg.frame)
    }

    /// The next frame every window gets, as (eval id, kind), failing the test after 30 s.
    async fn next(bus: &mut tokio::sync::broadcast::Receiver<crate::sidecars::repl::ReplFrameMsg>) -> (u64, String) {
        let (eval_id, frame) = next_frame(bus).await;
        let kind = frame["kind"].as_str().unwrap_or("").to_string();
        let kind = match frame["state"].as_str() {
            Some(state) => format!("{kind} {state}"),
            None => kind,
        };
        (eval_id, kind)
    }

    /// Every frame every window gets until `dones` runs are closed, and the message of each `error` frame among them,
    /// each checked to come before its run's `done`.
    async fn until_closed(
        bus: &mut tokio::sync::broadcast::Receiver<crate::sidecars::repl::ReplFrameMsg>,
        dones: usize,
    ) -> (Vec<(u64, serde_json::Value)>, Vec<String>) {
        let mut seen: Vec<(u64, serde_json::Value)> = Vec::new();
        while seen.iter().filter(|(_, f)| f["kind"] == "done").count() < dones {
            seen.push(next_frame(bus).await);
        }
        let mut said = Vec::new();
        for (at, (id, frame)) in seen.iter().enumerate() {
            if frame["kind"] == "error" {
                let closed = seen[at..].iter().any(|(i, f)| i == id && f["kind"] == "done");
                assert!(closed, "the reason comes before its run's done: {seen:?}");
                said.push(frame["message"].as_str().unwrap_or("").to_string());
            }
        }
        (seen, said)
    }

    /// The payload `sot-fe repl run <ws> <path> --fresh` sends (agents/sot-fe/sot-fe-repl.sh `send_repl_run_file`).
    fn fresh(r: &Row) -> serde_json::Value {
        json!({ "workspace_id": r.id, "path": r.script, "fresh": true })
    }

    #[tokio::test]
    async fn a_fresh_run_is_announced_to_every_window_under_the_daemons_number() {
        let mut r = row(ANSWERS);
        let out = handle_repl_run_file(1, fresh(&r), &r.session, &r.workspaces, &Default::default()).await.expect("handler");
        let ack = out[0].0.payload.clone();
        assert_eq!(ack["accepted"], true, "{ack}");
        let mut seen = Vec::new();
        while !seen.iter().any(|(_, kind): &(u64, String)| kind == "done") {
            seen.push(next(&mut r.bus).await);
        }
        println!("repl-fresh: every window got {seen:?}; the ack's eval_id is {}", ack["eval_id"]);
        let Some(started) = seen.iter().position(|(_, kind)| kind == "started") else {
            panic!("no started frame: no window makes a drawer entry for the run, so its output reaches none: {seen:?}");
        };
        let number = seen[started].0;
        assert!(number >= 1 << 40, "the run's number is the daemon's, never a window's own: {seen:?}");
        let output: Vec<&(u64, String)> = seen.iter().filter(|(_, kind)| kind == "stdout" || kind == "done").collect();
        assert!(output.iter().all(|(id, _)| *id == number), "the output and done come under the announced number: {seen:?}");
        let first = seen.iter().position(|(_, kind)| kind == "stdout").expect("the run's output");
        assert!(started < first, "the announcement comes before the output: {seen:?}");
        assert_eq!(ack["eval_id"], number, "the ack names the announced run");
    }

    #[tokio::test]
    async fn a_run_waiting_when_fresh_restarts_the_repl_is_answered_and_closed() {
        let mut r = row(BUSY);
        let first = json!({ "workspace_id": r.id, "eval_id": 1, "code": "1" });
        handle_repl_eval(1, first, &r.session, &r.workspaces, &Default::default()).await.expect("setup: the eval starts the child");
        let state = || r.workspaces.resolve(Some(&r.id)).map(|w| w.repl_state()).unwrap_or("none");
        within(Duration::from_secs(30), "the child is ready", || state() == "ready").await;
        let waiting = json!({ "workspace_id": r.id, "input": { "kind": "eval", "code": "while true end" }, "timeout_ms": 60_000 });
        let t0 = std::time::Instant::now();
        let execute = crate::sidecars::repl::execute::handle_repl_execute(2, waiting, &r.session, &r.workspaces);
        // The execute announces its run (`started`) just before it submits, and the slot lock is first come, first served:
        // a restart that begins after the announcement waits behind the submit.
        let mut announced = r.bus.resubscribe();
        let restart = async {
            while next_frame(&mut announced).await.1["kind"] != "started" {}
            handle_repl_run_file(3, fresh(&r), &r.session, &r.workspaces, &Default::default()).await.expect("handler")
        };
        let (answered, restarted) = tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(execute, restart) })
            .await
            .expect("the waiting run is answered within 20 s of a --fresh");
        let waited = t0.elapsed();
        let answer = answered.expect("handler")[0].0.payload.clone();
        assert_eq!(restarted[0].0.payload["accepted"], true, "{}", restarted[0].0.payload);
        // Two runs were cut short: the window's eval and the waiting execute. Each entry closes with the reason.
        let (seen, said) = until_closed(&mut r.bus, 2).await;
        println!("repl-retire: the waiting run answered {answer} after {waited:?}; every window got {seen:?}");
        assert_eq!(answer["outcome"], "restarted", "the answer names the restart: {answer}");
        let error = answer["error"]["message"].as_str().unwrap_or("");
        assert!(error.contains("restarted"), "the answer says why: {answer}");
        assert_eq!(said.len(), 2, "each run's entry says why it closed: {said:?}");
        assert!(said.iter().all(|m| m.contains("restarted") && !m.contains("died")), "a restart is no death: {said:?}");
    }

    /// A restart is the owner's own act, not a failure: the retired child's stderr tail is not logged at warn. (A death
    /// still logs it: `contract_tests::a_repl_child_that_fails_its_start_logs_its_stderr_at_warn`.)
    #[tokio::test]
    async fn a_restart_logs_no_stderr_tail_at_warn() {
        let log = sot_log::test_log::capture();
        let r = row(BUSY_NOISY);
        let first = json!({ "workspace_id": r.id, "eval_id": 1, "code": "1" });
        handle_repl_eval(1, first, &r.session, &r.workspaces, &Default::default()).await.expect("setup: the eval starts the child");
        let state = || r.workspaces.resolve(Some(&r.id)).map(|w| w.repl_state()).unwrap_or("none");
        within(Duration::from_secs(30), "the child is ready", || state() == "ready").await;
        let out = handle_repl_run_file(2, fresh(&r), &r.session, &r.workspaces, &Default::default()).await.expect("handler");
        assert_eq!(out[0].0.payload["accepted"], true, "{}", out[0].0.payload);
        assert!(!crate::sidecars::warned(&log.text(), "repl-restart-red"), "a restart logged the old child's tail at warn: {}", log.text());
    }

    /// A window's run is named running (`CallerNumbers::running`, what its hello reply names) from the supervisor's
    /// write of it to the child until the run ends, here by the child's death: the close-out marks each run ended
    /// before it publishes the run's `done`, so once every window has its `done`, no hello names it.
    #[tokio::test]
    async fn a_windows_run_is_named_running_from_its_write_until_its_end() {
        let mut r = row(DIES);
        let window = CallerNumbers::default();
        let first = json!({ "workspace_id": r.id, "eval_id": 7, "code": "1" });
        handle_repl_eval(1, first, &r.session, &r.workspaces, &window).await.expect("setup: the eval starts the child");
        within(Duration::from_secs(30), "the child has read the window's eval", || window.running(&r.bus) == [7]).await;
        let second = json!({ "workspace_id": r.id, "eval_id": 8, "code": "2" });
        handle_repl_eval(2, second, &r.session, &r.workspaces, &window).await.expect("handler");
        let (seen, _) = until_closed(&mut r.bus, 2).await;
        println!("repl-running: after the death every window got {seen:?}; a hello names {:?}", window.running(&r.bus));
        assert!(window.running(&r.bus).is_empty(), "a run whose done is out is named no more: {:?}", window.running(&r.bus));
    }

    #[tokio::test]
    async fn a_run_waiting_when_the_repl_dies_is_answered_died() {
        let mut r = row(DIES);
        let first = json!({ "workspace_id": r.id, "eval_id": 1, "code": "1" });
        handle_repl_eval(1, first, &r.session, &r.workspaces, &Default::default()).await.expect("setup: the eval starts the child");
        let state = || r.workspaces.resolve(Some(&r.id)).map(|w| w.repl_state()).unwrap_or("none");
        within(Duration::from_secs(30), "the child is ready", || state() == "ready").await;
        let waiting = json!({ "workspace_id": r.id, "input": { "kind": "eval", "code": "1" }, "timeout_ms": 60_000 });
        let execute = crate::sidecars::repl::execute::handle_repl_execute(2, waiting, &r.session, &r.workspaces);
        let answered = tokio::time::timeout(Duration::from_secs(20), execute).await.expect("answered within 20 s");
        let answer = answered.expect("handler")[0].0.payload.clone();
        let (seen, said) = until_closed(&mut r.bus, 2).await;
        println!("repl-retire: a death answered {answer}; every window got {seen:?}");
        assert_eq!(answer["outcome"], "repl_died", "{answer}");
        let error = answer["error"]["message"].as_str().unwrap_or("");
        assert!(error.contains("died"), "the answer says why: {answer}");
        assert_eq!(said.len(), 2, "each run's entry says why it closed: {said:?}");
        assert!(said.iter().all(|m| m.contains("died")), "a death still says died: {said:?}");
    }
}
