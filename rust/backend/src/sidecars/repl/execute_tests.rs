//! `repl.execute` against a `/bin/sh` stand-in for the Julia REPL child: pins every reply and the drawer frames.

use super::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A `julia` stand-in speaking the shim's line protocol: it logs each request line, then acts on the code or path text.
const STUB: &str = r#"#!/bin/sh
while read -r line; do
  printf '%s\n' "$line" >> "$0.log"
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  e=$(printf '%s' "$line" | sed -e 's/.*"eval_id":\([0-9]*\).*/\1/')
  frame() { printf '{"v":1,"id":0,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":%s}}\n' "$e" "$1"; }
  res() { printf '{"v":1,"id":%s,"kind":"res","op":"repl.eval","payload":%s}\n' "$id" "$1"; }
  case "$line" in
    *case-hang*) ;;
    *case-die*) exit 0 ;;
    *case-error*) frame '{"kind":"error","message":"boom"}'; frame '{"kind":"done"}'; res '{}' ;;
    *case-busy*) frame '{"kind":"error","message":"REPL busy"}'; frame '{"kind":"done"}'; res '{}' ;;
    *case-rescode*) res '{"code":"io_error","error":"nope"}' ;;
    *case-ok*)
      frame '{"kind":"stdout","text":"hi"}'
      frame '{"kind":"stderr","text":"warn"}'
      frame '{"kind":"value","mime":"text/plain","text":"42"}'
      frame '{"kind":"image","mime":"image/png","data_base64":"iVBORw0KGgo="}'
      frame '{"kind":"done"}'
      res '{"project_dir":"/p","project_source":"s"}' ;;
    *) frame '{"kind":"done"}'; res '{}' ;;
  esac
done
"#;

/// `SOT_JULIA_BIN` and `SOT_RESOURCE_ROOT` are process-global: pinning them takes `paths::ENV_TEST_LOCK`.
struct EnvPin {
    _serial: std::sync::MutexGuard<'static, ()>,
    julia_bin: Option<std::ffi::OsString>,
    resource_root: Option<std::ffi::OsString>,
}

impl Drop for EnvPin {
    fn drop(&mut self) {
        for (key, val) in [("SOT_JULIA_BIN", &self.julia_bin), ("SOT_RESOURCE_ROOT", &self.resource_root)] {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn pin_env(julia_bin: &Path, resource_root: &Path) -> EnvPin {
    let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pin = EnvPin {
        _serial: serial,
        julia_bin: std::env::var_os("SOT_JULIA_BIN"),
        resource_root: std::env::var_os("SOT_RESOURCE_ROOT"),
    };
    std::env::set_var("SOT_JULIA_BIN", julia_bin);
    std::env::set_var("SOT_RESOURCE_ROOT", resource_root);
    pin
}

fn scratch_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("sot-repl-execute-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// Register a row named `label` whose root is a fresh folder with no Project.toml; returns (id, root).
fn add_row(workspaces: &Workspaces, base: &Path, label: &str) -> (String, PathBuf) {
    let root = base.join(label);
    std::fs::create_dir_all(&root).unwrap();
    let ws = crate::workspaces::Workspace::from_label(label, root.clone(), false, "none".into(), String::new(), String::new());
    let id = ws.workspace_id.clone();
    workspaces.insert(ws);
    (id, root)
}

/// Run `repl.execute` and return the reply's payload.
async fn execute(workspaces: &Workspaces, session: &Session, payload: Value) -> Value {
    let out = handle_repl_execute(7, payload, session, workspaces).await.expect("handler");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0.op, op::REPL_EXECUTE);
    out[0].0.payload.clone()
}

/// The reply with its two run-dependent fields (`elapsed_ms`, `run_id`) taken out; returns them.
fn strip_run(reply: &mut Value) -> (u64, String) {
    let obj = reply.as_object_mut().unwrap();
    let elapsed = obj.remove("elapsed_ms").and_then(|v| v.as_u64()).unwrap();
    let run_id = obj.remove("run_id").and_then(|v| v.as_str().map(String::from)).unwrap();
    (elapsed, run_id)
}

fn eval_payload(id: &str, code: &str, timeout_ms: Option<u64>) -> Value {
    let mut p = json!({ "workspace_id": id, "input": { "kind": "eval", "code": code } });
    if let Some(t) = timeout_ms {
        p["timeout_ms"] = json!(t);
    }
    p
}

fn run_file_payload(id: &str, path: &str) -> Value {
    json!({ "workspace_id": id, "input": { "kind": "run_file", "path": path } })
}

/// The frames the bus carried for `eval_id` since the last drain, in order.
fn drain(rx: &mut tokio::sync::broadcast::Receiver<ReplFrameMsg>, eval_id: u64) -> Vec<ReplFrameMsg> {
    let mut got = Vec::new();
    while let Ok(m) = rx.try_recv() {
        if m.eval_id == eval_id {
            got.push(m);
        }
    }
    got
}

/// The refusal reply `exec_err_frame` writes, as JSON without `elapsed_ms` and `run_id`.
fn refusal(id: &str, outcome: &str, message: &str) -> Value {
    json!({
        "workspace_id": id, "outcome": outcome, "stdout": "", "stderr": "", "values": [],
        "error": { "message": message, "stacktrace": [] }, "figures": [], "truncated": false,
    })
}

fn logged_request(log: &Path, eval_id: u64) -> Value {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["payload"]["eval_id"].as_u64() == Some(eval_id))
        .unwrap_or_else(|| panic!("no request logged for eval {eval_id}"))
}

fn assert_started_first(frames: &[ReplFrameMsg], run_id: &str, slug: &str, display: &str) {
    let first = frames.first().expect("a started frame");
    assert_eq!(first.workspace_id.as_deref(), Some(slug));
    assert_eq!(first.frame, json!({ "kind": "started", "run_id": run_id, "origin": "session", "display": display }));
}

#[tokio::test]
async fn repl_execute_reports_are_unchanged() {
    let dir = scratch_dir();
    let resources = dir.join("resources");
    std::fs::create_dir_all(resources.join("julia").join("repl")).unwrap();
    let julia = dir.join("julia");
    std::fs::write(&julia, STUB).unwrap();
    std::fs::set_permissions(&julia, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = dir.join("julia.log");
    let _pin = pin_env(&julia, &resources);

    let (frame_tx, mut bus) = tokio::sync::broadcast::channel(256);
    let workspaces = Workspaces::new();
    workspaces.set_repl_frame_tx(frame_tx);
    let session = Session::new();

    // ok: every frame kind, a figure, the project the shim reported.
    let (id, root) = add_row(&workspaces, &dir, "okrow");
    let mut reply = execute(&workspaces, &session, eval_payload(&id, "case-ok", None)).await;
    let (_, run_id) = strip_run(&mut reply);
    let eval_id: u64 = run_id.strip_prefix("exec-").unwrap().parse().unwrap();
    let fig = root.join(".sot").join("runs").join(&run_id).join("fig-0.png");
    assert_eq!(
        reply,
        json!({
            "workspace_id": id, "outcome": "ok", "stdout": "hi", "stderr": "warn",
            "values": [{ "mime": "text/plain", "text": "42" }], "figures": [fig.to_string_lossy()],
            "truncated": false, "project_dir": "/p", "project_source": "s",
        })
    );
    assert_eq!(std::fs::read(&fig).unwrap(), [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    assert_started_first(&drain(&mut bus, eval_id), &run_id, "okrow", "case-ok");
    let logged = logged_request(&log, eval_id);
    assert_eq!(logged["op"], "repl.eval");
    assert_eq!(logged["payload"], json!({ "code": "case-ok", "eval_id": eval_id, "workspace_id": id }));

    // error: the first error frame becomes the report's error.
    let (id, _) = add_row(&workspaces, &dir, "errrow");
    let mut reply = execute(&workspaces, &session, eval_payload(&id, "case-error", None)).await;
    strip_run(&mut reply);
    assert_eq!(
        reply,
        json!({
            "workspace_id": id, "outcome": "error", "stdout": "", "stderr": "", "values": [],
            "error": { "message": "boom", "stacktrace": [] }, "figures": [], "truncated": false,
        })
    );

    // busy.
    let (id, _) = add_row(&workspaces, &dir, "busyrow");
    let mut reply = execute(&workspaces, &session, eval_payload(&id, "case-busy", None)).await;
    strip_run(&mut reply);
    assert_eq!(reply["outcome"], "busy");
    assert_eq!(reply["error"], json!({ "message": "REPL busy", "stacktrace": [] }));

    // a res carrying a code is an error whatever the frames said.
    let (id, _) = add_row(&workspaces, &dir, "coderow");
    let mut reply = execute(&workspaces, &session, eval_payload(&id, "case-rescode", None)).await;
    strip_run(&mut reply);
    assert_eq!(reply, refusal(&id, "error", "nope"));

    // timeout: the run is left going, the drawer entry is closed by a done frame.
    let (id, _) = add_row(&workspaces, &dir, "hangrow");
    let mut reply = execute(&workspaces, &session, eval_payload(&id, "case-hang", Some(1000))).await;
    let (elapsed, run_id) = strip_run(&mut reply);
    let eval_id: u64 = run_id.strip_prefix("exec-").unwrap().parse().unwrap();
    assert!(elapsed >= 1000);
    assert_eq!(reply["outcome"], "timeout");
    assert!(reply.get("error").is_none());
    let frames = drain(&mut bus, eval_id);
    assert_started_first(&frames, &run_id, "hangrow", "case-hang");
    assert_eq!(frames.last().unwrap().frame, json!({ "kind": "done", "eval_id": eval_id, "elapsed_ms": elapsed }));

    // repl_died: the child exits without answering.
    let (id, _) = add_row(&workspaces, &dir, "diedrow");
    let mut reply = execute(&workspaces, &session, eval_payload(&id, "case-die", None)).await;
    let (elapsed, run_id) = strip_run(&mut reply);
    let eval_id: u64 = run_id.strip_prefix("exec-").unwrap().parse().unwrap();
    assert_eq!(reply["outcome"], "repl_died");
    let frames = drain(&mut bus, eval_id);
    assert_started_first(&frames, &run_id, "diedrow", "case-die");
    assert_eq!(frames.last().unwrap().frame, json!({ "kind": "done", "eval_id": eval_id, "elapsed_ms": elapsed }));

    // run_file: the canonical path is what the REPL is asked to run.
    let (id, root) = add_row(&workspaces, &dir, "filerow");
    std::fs::write(root.join("x.jl"), "1\n").unwrap();
    std::fs::write(root.join("notes.txt"), "n\n").unwrap();
    let mut reply = execute(&workspaces, &session, run_file_payload(&id, "x.jl")).await;
    let (_, run_id) = strip_run(&mut reply);
    let eval_id: u64 = run_id.strip_prefix("exec-").unwrap().parse().unwrap();
    assert_eq!(reply["outcome"], "ok");
    assert_started_first(&drain(&mut bus, eval_id), &run_id, "filerow", "run x.jl");
    let logged = logged_request(&log, eval_id);
    assert_eq!(logged["op"], "repl.run_file");
    assert_eq!(
        logged["payload"],
        json!({ "eval_id": eval_id, "fresh": false, "path": root.join("x.jl").to_string_lossy(), "workspace_id": id })
    );

    // Refusals before the REPL is asked anything.
    let reply = execute(&workspaces, &session, json!({ "workspace_id": id })).await;
    assert_eq!(reply["code"], "bad_request");
    assert!(reply["error"].as_str().unwrap().starts_with("bad repl.execute payload: "));
    let reply = execute(&workspaces, &session, eval_payload("nope", "1", None)).await;
    assert_eq!(reply, json!({ "error": "unknown workspace: nope", "code": "unknown_workspace" }));

    let outside = dir.join("outside.jl");
    std::fs::write(&outside, "1\n").unwrap();
    let abs = outside.to_string_lossy().to_string();
    let mut reply = execute(&workspaces, &session, run_file_payload(&id, &abs)).await;
    strip_run(&mut reply);
    let message = format!(
        "repl run is confined to the workspace root ({}); {abs} is outside it — use `repl eval --code 'include(\"{abs}\")'` for files elsewhere",
        root.display()
    );
    assert_eq!(reply, refusal(&id, "error", &message));

    let mut reply = execute(&workspaces, &session, run_file_payload(&id, "notes.txt")).await;
    strip_run(&mut reply);
    let message = format!("not an existing .jl file: {}", root.join("notes.txt").display());
    assert_eq!(reply, refusal(&id, "error", &message));

    let mut reply = execute(&workspaces, &session, run_file_payload(&id, "gone.jl")).await;
    strip_run(&mut reply);
    let message = "cannot resolve path \"gone.jl\": No such file or directory (os error 2)";
    assert_eq!(reply, refusal(&id, "error", message));

    let _ = std::fs::remove_dir_all(&dir);
}
