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
                    "code": "repl_eval_failed",
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
    if fresh {
        let Some(ref p) = path_str else {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::REPL_RUN_FILE,
                    json!({
                        "error": "fresh=true requires a path",
                        "code": "bad_request",
                    }),
                ),
                None,
            )]);
        };
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

    // Fire-and-forget: queue the run and ack immediately. Frames stream as
    // `repl.frame` evts; the frontend keys completion off the `done` frame.
    if let Err(e) = repl.submit(op::REPL_RUN_FILE, forwarded_payload).await {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_RUN_FILE,
                json!({
                    "error": format!("{e:#}"),
                    "code": "repl_run_file_failed",
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
                "eval_id": eval_id,
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
            "code": "repl_interrupt_failed",
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
    use crate::sidecars::contract_tests::within;
    use std::time::Duration;

    /// Holds `paths::ENV_TEST_LOCK` and puts back the two process-global variables this test sets, however it ends.
    struct EnvPin {
        _serial: std::sync::MutexGuard<'static, ()>,
        saved: [(&'static str, Option<std::ffi::OsString>); 2],
    }

    impl Drop for EnvPin {
        fn drop(&mut self) {
            for (key, val) in &self.saved {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
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
        let _pin = EnvPin {
            _serial: crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
            saved: ["SOT_JULIA_BIN", "SOT_RESOURCE_ROOT"].map(|k| (k, std::env::var_os(k))),
        };
        std::env::set_var("SOT_JULIA_BIN", &julia);
        std::env::set_var("SOT_RESOURCE_ROOT", &resources);

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
