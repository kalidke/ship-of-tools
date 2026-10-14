#![cfg(target_os = "linux")]
//! Two windows on one row, through a real `sotd` whose REPL child is a shell stand-in. Each window numbers its own
//! eval 1, as every window's first eval is. A window is written its own run under its own number and every other
//! window's run under the daemon's number, after the run's `started` frame, so no window's entry collects another
//! window's output (a drawer appends a frame to the entry with the frame's number, and makes an entry for a run only
//! from a `started` frame: `rust/frontend/src/ui/drawer/repl/replies.rs`). A `repl.execute` run shows in every window,
//! and so does the output of one that timed out, after the run's entry was closed (ADR 0033 phase 2).

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use serde_json::{json, Value};
use sot_protocol::{codec, op, Frame, Kind};
use support::{connect_and_hello, Conn, Env, BOUND};

/// A `julia` stand-in: it says it is ready, then answers each request with one stdout frame holding the request's
/// code, its `done` and its `res`, under the request's `eval_id`. A code starting `late` first waits 3 s, past the 1 s
/// its caller waits.
const STUB: &str = r#"#!/bin/sh
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  eid=$(printf '%s' "$line" | sed -n -e 's/.*"eval_id":\([0-9]*\).*/\1/p')
  code=$(printf '%s' "$line" | sed -n -e 's/.*"code":"\([^"]*\)".*/\1/p')
  case "$code" in late*) sleep 3 ;; esac
  printf '{"v":1,"id":%s,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"stdout","text":"%s"}}}\n' "$id" "$eid" "$code"
  printf '{"v":1,"id":%s,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"done","eval_id":%s,"elapsed_ms":1}}}\n' "$id" "$eid" "$eid"
  printf '{"v":1,"id":%s,"kind":"res","op":"repl.eval","payload":{}}\n' "$id"
done
"#;

/// One connection and every `repl.frame` it has been written, as (eval id, frame).
struct Window {
    conn: Conn,
    next_id: u64,
    frames: Vec<(u64, Value)>,
}

impl Window {
    async fn open(env: &Env) -> Self {
        let (conn, next_id) = connect_and_hello(&env.socket_path).await;
        Self { conn, next_id, frames: Vec::new() }
    }

    /// Reads one frame, keeping a `repl.frame`; returns any other frame.
    async fn read_one(&mut self) -> Frame {
        let (frame, _blob) = tokio::time::timeout(BOUND, codec::read_frame(&mut self.conn))
            .await
            .unwrap_or_else(|_| panic!("nothing within {BOUND:?}; repl frames so far: {:?}", self.frames))
            .expect("read_frame");
        if frame.kind == Kind::Evt && frame.op == op::REPL_FRAME {
            let eval_id = frame.payload["eval_id"].as_u64().expect("a repl.frame names its eval");
            self.frames.push((eval_id, frame.payload["frame"].clone()));
        }
        frame
    }

    /// Sends a request and returns its reply's payload, keeping every `repl.frame` read before it.
    async fn request(&mut self, op_name: &str, payload: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        codec::write_frame(&mut self.conn, &Frame::req(id, op_name, payload), None).await.expect("write_frame");
        loop {
            let frame = self.read_one().await;
            if frame.id == id && frame.kind != Kind::Evt {
                return frame.payload;
            }
        }
    }

    /// Reads until a `repl.frame` holding `text` has been written to this window.
    async fn until_output(&mut self, text: &str) {
        while !self.frames.iter().any(|(_, f)| f["text"] == text) {
            self.read_one().await;
        }
    }

    /// The kinds and texts of the frames under `eval_id`, in order.
    fn under(&self, eval_id: u64) -> Vec<String> {
        let shown = |f: &Value| format!("{} {}", f["kind"].as_str().unwrap_or(""), f["text"].as_str().unwrap_or("")).trim().to_string();
        self.frames.iter().filter(|(id, _)| *id == eval_id).map(|(_, f)| shown(f)).collect()
    }

    /// The number of the run a `started` frame announced with `display`, if one did.
    fn announced(&self, display: &str) -> Option<u64> {
        self.frames.iter().find(|(_, f)| f["kind"] == "started" && f["display"] == display).map(|(id, _)| *id)
    }
}

#[tokio::test]
async fn two_windows_on_one_row_each_see_their_own_runs_and_every_announced_one() {
    let env = Env::new("replwin");
    let folder = env.daemon_project_root.parent().expect("the env's folder").to_path_buf();
    let stub = folder.join("stub-julia");
    sot_log::test_exec::write_executable(&stub, STUB);
    let bin = folder.join("bin");
    std::fs::create_dir_all(&bin).expect("an empty PATH entry");
    env.spawn_sotd_with_path_and_env(&bin, &[("SOT_JULIA_BIN", &stub.to_string_lossy())]);

    let mut a = Window::open(&env).await;
    let mut b = Window::open(&env).await;
    let mut cli = Window::open(&env).await;
    let list = cli.request(op::WORKSPACE_LIST, json!({})).await;
    let row = list["workspaces"]
        .as_array()
        .and_then(|rows| rows.iter().find(|w| w["is_default"].as_bool() == Some(true)))
        .and_then(|w| w["workspace_id"].as_str())
        .expect("the default row")
        .to_string();

    // Each window's first eval is its eval 1.
    let ack = a.request(op::REPL_EVAL, json!({ "eval_id": 1, "code": "from-a" })).await;
    assert_eq!(ack["eval_id"], 1, "{ack}");
    let ack = b.request(op::REPL_EVAL, json!({ "eval_id": 1, "code": "from-b" })).await;
    assert_eq!(ack["eval_id"], 1, "{ack}");
    let ran = cli
        .request(op::REPL_EXECUTE, json!({ "workspace_id": row, "input": { "kind": "eval", "code": "from-cli" } }))
        .await;
    assert_eq!(ran["outcome"], "ok", "{ran}");
    let late = json!({ "workspace_id": row, "input": { "kind": "eval", "code": "late-cli" }, "timeout_ms": 1000 });
    let ran = cli.request(op::REPL_EXECUTE, late).await;
    assert_eq!(ran["outcome"], "timeout", "{ran}");

    for (w, own, other) in [(&mut a, "from-a", "from-b"), (&mut b, "from-b", "from-a")] {
        w.until_output("late-cli").await;
        println!("repl-windows: the window that ran {own} was written {:?}", w.frames);
        assert_eq!(w.under(1), [format!("stdout {own}"), "done".to_string()], "the window's eval 1 holds its own output alone");
        assert_eq!(w.announced(own), None, "a window is never announced its own run");
        let theirs = w.announced(other).expect("the other window's run is announced");
        assert!(theirs >= 1 << 40, "under the daemon's number: {theirs}");
        let want = vec!["started".to_string(), format!("stdout {other}"), "done".to_string()];
        assert_eq!(w.under(theirs), want, "the other window's run");
        let cli_run = w.announced("from-cli").expect("the repl.execute run is announced");
        assert_eq!(w.under(cli_run), ["started", "stdout from-cli", "done"], "the repl.execute run");
        let late_run = w.announced("late-cli").expect("the timed-out run is announced");
        let shown = w.under(late_run);
        assert_eq!(shown[..2], ["started", "done"], "the timed-out run's entry is closed when its caller stops waiting");
        assert!(shown[2..].iter().any(|s| s == "stdout late-cli"), "its later output still comes: {shown:?}");
    }
    env.kill_daemon_bounded().await;
}
