//! The REPL ops: `repl.eval`, `repl.run_file`, `repl.interrupt` on a row's REPL child.

use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use crate::session::Session;
use crate::rows::row_or_reply;
use crate::rows::Workspaces;
use crate::server::reply::HandlerOutput;

pub async fn handle_repl_eval(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
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
    // Fire-and-forget: queue the eval at the supervisor and return an ack
    // immediately. The eval's frames (stdout/value/error/done) stream as
    // separate `repl.frame` evts over the broadcast bus; the frontend keys
    // completion off the terminal `done` frame, not this ack. Returning here
    // (instead of awaiting the eval) keeps the connection loop free to read a
    // mid-eval `repl.interrupt`.
    if let Err(e) = repl.submit(op::REPL_EVAL, payload_json.clone()).await {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_EVAL,
                json!({
                    "error": format!("{e:#}"),
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
            json!({ "eval_id": eval_id, "elapsed_ms": 0, "frames": [], "accepted": true }),
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

    // Priority J: `r` in NavTree maps to fresh=true. Resolve the file's
    // closest-ancestor Project.toml *here* on Rust, bounce the REPL
    // supervisor into that project, then forward a fresh=false submission
    // so the Julia side just `include`s in the now-correct env. The
    // dead-code subprocess branch in ShipToolsRepl.handle_run_file never
    // runs anymore (commented in the Julia source).
    //
    // With streamed frames (Option B), the run no longer rides a single
    // response: the Julia child emits the include's stdout/value/error/done
    // as `repl.frame` evts over the broadcast bus, and this handler returns
    // only an immediate ack. The reset banner that previously rode the
    // response as a synthetic stderr frame is now the Julia shim's job to
    // emit as a frame (or it shows up implicitly via the fresh env), so the
    // Rust-side response post-processing (project_dir override, banner
    // prepend) is gone — there's no response payload to fold it into.
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
        let abs_path = {
            let pb = std::path::PathBuf::from(p);
            if pb.is_absolute() {
                pb
            } else {
                std::env::current_dir().unwrap_or_default().join(pb)
            }
        };
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

    // A caller that numbers no run (`sot-fe repl run --fresh` sends no `eval_id`) has no drawer entry for it, and a
    // window makes one only for its own evals and for a `started` frame. So the daemon numbers the run from
    // `repl.execute`'s space and announces it to every window, after the restart and before the run (ADR 0033 phase
    // 2); the shim's `done`, or the close-out of a child that ends first, closes it.
    let announced = match eval_id {
        Some(_) => None,
        None => {
            let id = super::execute::next_exec_eval_id();
            if let Some(obj) = forwarded_payload.as_object_mut() {
                obj.insert("eval_id".to_string(), json!(id));
            }
            let name = std::path::Path::new(p).file_name().and_then(|s| s.to_str()).unwrap_or("?.jl");
            let display = if fresh { format!("run {name} (fresh)") } else { format!("run {name}") };
            let origin = payload_json.get("origin").and_then(|v| v.as_str());
            Some((id, super::execute::announce_exec_started(origin, &ws, workspaces, id, &format!("exec-{id}"), display)))
        }
    };

    // Fire-and-forget: queue the run and ack immediately. Frames stream as
    // `repl.frame` evts; the frontend keys completion off the `done` frame.
    if let Err(e) = repl.submit(op::REPL_RUN_FILE, forwarded_payload).await {
        if let Some((id, (frame_ws, frame_tx))) = &announced {
            super::execute::close_exec_run(frame_tx, frame_ws, *id, 0);
        }
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_RUN_FILE,
                json!({
                    "error": format!("{e:#}"),
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
                "eval_id": eval_id.or(announced.as_ref().map(|(id, _)| *id)),
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
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
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

/// The daemon forwards an interrupt's payload to the REPL child unchanged, so the
/// evals it names (`eval_ids`) reach the shim, which decides which eval it means.
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
        handle_repl_eval(1, json!({ "workspace_id": id, "eval_id": 7, "code": "1" }), &session, &workspaces)
            .await
            .expect("setup: the eval starts the child");
        within(Duration::from_secs(30), "the child is ready", || row.repl_state() == "ready").await;

        let mut answers = Vec::new();
        for req in 2..4 {
            let out = handle_repl_interrupt(req, json!({ "workspace_id": id, "eval_ids": [7] }), &session, &workspaces)
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
        handle_repl_eval(1, json!({ "workspace_id": id, "eval_id": 205, "code": "1" }), &session, &workspaces)
            .await
            .expect("setup: the eval starts the child");
        within(Duration::from_secs(30), "the child is ready", || row.repl_state() == "ready").await;

        let named = json!({ "workspace_id": id, "eval_ids": [205, 1u64 << 40] });
        let out = handle_repl_interrupt(2, named.clone(), &session, &workspaces).await.expect("handler");
        assert_eq!(out[0].0.payload, json!({ "interrupted": true }), "the child's answer is the reply");
        let logged: Vec<serde_json::Value> = std::fs::read_to_string(dir.join("julia.log"))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .filter(|v: &serde_json::Value| v["op"] == op::REPL_INTERRUPT)
            .collect();
        assert_eq!(logged.len(), 1, "one interrupt reached the child: {logged:?}");
        assert_eq!(logged[0]["payload"], named, "the child received the payload as sent");
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

    /// A `julia` stand-in: it says it is ready, then never reads its input.
    const DEAF: &str = "#!/bin/sh\nprintf '{\"v\":1,\"id\":0,\"kind\":\"evt\",\"op\":\"repl.ready\",\"payload\":{}}\\n'\nexec sleep 60\n";

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
        let tmp = tempfile::Builder::new().prefix("sot-repl-busy-").tempdir().unwrap();
        let dir = tmp.path();
        let resources = dir.join("resources");
        std::fs::create_dir_all(resources.join("julia").join("repl")).unwrap();
        let root = dir.join("row");
        std::fs::create_dir_all(&root).unwrap();
        let julia = dir.join("julia");
        sot_log::test_exec::write_executable(&julia, DEAF);
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
        let started = bounded("eval 0", handle_repl_eval(0, first, &session, &workspaces)).await;
        assert_eq!(code_of(&started.expect("handler")), "accepted");
        within(Duration::from_secs(30), "the child is ready", || row.repl_state() == "ready").await;
        Deaf { session, workspaces, id, _pin: pin, _tmp: tmp }
    }

    #[tokio::test]
    async fn an_unanswered_interrupt_ends_at_its_bound() {
        let row = deaf_row().await;
        let t0 = std::time::Instant::now();
        let interrupt = handle_repl_interrupt(1, json!({ "workspace_id": row.id }), &row.session, &row.workspaces);
        let unanswered = bounded("an interrupt", interrupt).await.expect("handler");
        let waited = t0.elapsed();
        assert_eq!(code_of(&unanswered), "repl_interrupt_failed");
        let error = unanswered[0].0.payload["error"].as_str().unwrap_or("").to_string();
        assert!(error.contains("--fresh"), "the error names the restart that ends a non-yielding eval: {error}");
        let bound = Duration::from_secs(10);
        assert!(waited >= bound && waited < bound + Duration::from_secs(5), "{waited:?}");
        println!("repl-busy: an unanswered interrupt -> repl_interrupt_failed after {waited:?}");
    }

    #[tokio::test]
    async fn a_full_queue_is_refused_at_once() {
        let row = deaf_row().await;
        let (session, workspaces, id) = (&row.session, &row.workspaces, &row.id);
        let code = "x".repeat(64 * 1024);
        let mut codes = Vec::new();
        for n in 1..=20u64 {
            let eval = json!({ "workspace_id": id, "eval_id": n, "code": code });
            let out = bounded(&format!("eval {n}"), handle_repl_eval(n, eval, session, workspaces)).await;
            codes.push(code_of(&out.expect("handler")));
        }
        let busy = codes.iter().filter(|c| *c == "repl_busy").count();
        assert!(busy >= 1 && codes.iter().all(|c| c == "accepted" || c == "repl_busy"), "{codes:?}");
        let interrupt = handle_repl_interrupt(21, json!({ "workspace_id": id }), session, workspaces);
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
/// entry for it is closed.
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

    /// The next frame every window gets, as (eval id, kind), failing the test after 30 s.
    async fn next(bus: &mut tokio::sync::broadcast::Receiver<crate::sidecars::repl::ReplFrameMsg>) -> (u64, String) {
        let msg = tokio::time::timeout(Duration::from_secs(30), bus.recv())
            .await
            .expect("a frame within 30 s")
            .expect("the frame bus");
        let kind = msg.frame["kind"].as_str().unwrap_or("").to_string();
        let kind = match msg.frame["state"].as_str() {
            Some(state) => format!("{kind} {state}"),
            None => kind,
        };
        (msg.eval_id, kind)
    }

    /// The payload `sot-fe repl run <ws> <path> --fresh` sends (agents/sot-fe/sot-fe-repl.sh `send_repl_run_file`).
    fn fresh(r: &Row) -> serde_json::Value {
        json!({ "workspace_id": r.id, "path": r.script, "fresh": true })
    }

    #[tokio::test]
    async fn a_fresh_run_is_announced_to_every_window_under_the_daemons_number() {
        let mut r = row(ANSWERS);
        let out = handle_repl_run_file(1, fresh(&r), &r.session, &r.workspaces).await.expect("handler");
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
        handle_repl_eval(1, first, &r.session, &r.workspaces).await.expect("setup: the eval starts the child");
        let state = || r.workspaces.resolve(Some(&r.id)).map(|w| w.repl_state()).unwrap_or("none");
        within(Duration::from_secs(30), "the child is ready", || state() == "ready").await;
        let waiting = json!({ "workspace_id": r.id, "input": { "kind": "eval", "code": "while true end" }, "timeout_ms": 60_000 });
        let t0 = std::time::Instant::now();
        let execute = crate::sidecars::repl::execute::handle_repl_execute(2, waiting, &r.session, &r.workspaces);
        let restart = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            handle_repl_run_file(3, fresh(&r), &r.session, &r.workspaces).await.expect("handler")
        };
        let (answered, restarted) = tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(execute, restart) })
            .await
            .expect("the waiting run is answered within 20 s of a --fresh");
        let waited = t0.elapsed();
        let answer = answered.expect("handler")[0].0.payload.clone();
        assert_eq!(restarted[0].0.payload["accepted"], true, "{}", restarted[0].0.payload);
        assert_eq!(answer["outcome"], "repl_died", "{answer}");
        let run: u64 = answer["run_id"].as_str().and_then(|s| s.strip_prefix("exec-")).and_then(|n| n.parse().ok()).expect("run_id");
        let mut seen = Vec::new();
        while !seen.contains(&(run, "done".to_string())) {
            seen.push(next(&mut r.bus).await);
        }
        println!("repl-fresh: the waiting run answered outcome {} after {waited:?}; every window got {seen:?}", answer["outcome"]);
    }
}
