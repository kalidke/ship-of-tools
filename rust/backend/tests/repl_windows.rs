#![cfg(target_os = "linux")]
//! Two windows on one row, through a real `sotd` whose REPL child is a shell stand-in. Each window numbers its own
//! eval 1, as every window's first eval is. A window is written its own run under its own number and every other
//! window's run under the daemon's number, after the run's `started` frame, so no window's entry collects another
//! window's output (a drawer appends a frame to the entry with the frame's number, and makes an entry for a run only
//! from a `started` frame: `rust/frontend/src/ui/drawer/repl/replies.rs`). A `repl.execute` run shows in every window,
//! and so does the output of one that timed out, after the run's entry was closed (ADR 0033 phase 2). A window that
//! redials while its eval runs is the same window (its hello's `instance`): it is written the rest of the run under its
//! own number, and its Ctrl+C reaches the run.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use serde_json::{json, Value};
use sot_protocol::{codec, op, Frame, HelloReq, Kind};
use support::{call, connect_and_hello, poll_until, try_connect, Conn, Env, BOUND};

/// A `julia` stand-in: it says it is ready, then logs each request line beside itself (`stub-julia.log`) and answers
/// it: an interrupt with `interrupted`, any other request with one stdout frame holding the request's code, its `done`
/// and its `res`, under the request's `eval_id`. A code starting `late` is answered 3 s later, past the 1 s its caller
/// waits, while the stand-in reads on.
const STUB: &str = r#"#!/bin/sh
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
answer() {
  printf '{"v":1,"id":%s,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"stdout","text":"%s"}}}\n' "$1" "$2" "$3"
  printf '{"v":1,"id":%s,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"done","eval_id":%s,"elapsed_ms":1}}}\n' "$1" "$2" "$2"
  printf '{"v":1,"id":%s,"kind":"res","op":"repl.eval","payload":{}}\n' "$1"
}
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$0.log"
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  case "$line" in *'"op":"repl.interrupt"'*)
    printf '{"v":1,"id":%s,"kind":"res","op":"repl.interrupt","payload":{"interrupted":true}}\n' "$id"
    continue ;;
  esac
  eid=$(printf '%s' "$line" | sed -n -e 's/.*"eval_id":\([0-9]*\).*/\1/p')
  code=$(printf '%s' "$line" | sed -n -e 's/.*"code":"\([^"]*\)".*/\1/p')
  case "$code" in
    late*) (sleep 3; answer "$id" "$eid" "$code") & ;;
    *) answer "$id" "$eid" "$code" ;;
  esac
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

    /// A connection of the window `instance`, with the hello a window sends: role `fe`, its address and its
    /// `instance`, which every connection and redial of one window shares (`rust/frontend/src/net/identity.rs`).
    async fn open_as(env: &Env, instance: &str) -> Self {
        let stream = poll_until(|| try_connect(&env.socket_path), BOUND, "sotd's socket to accept a connection").await;
        let mut conn = tokio::io::BufReader::new(stream);
        let hello = HelloReq {
            instance: Some(instance.to_string()),
            name: Some("fe@host-a".to_string()),
            ..HelloReq::this_process(instance, "fe", Some("host-a".to_string())).expect("this process's account")
        };
        let reply = call(&mut conn, 1, op::HELLO, serde_json::to_value(&hello).expect("a hello")).await;
        assert!(reply.payload.get("error").is_none(), "hello refused: {:?}", reply.payload);
        Self { conn, next_id: 2, frames: Vec::new() }
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
    stub_daemon(&env);

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

/// The daemon running the stand-in, and the stand-in's log of the request lines it read.
fn stub_daemon(env: &Env) -> std::path::PathBuf {
    let folder = env.daemon_project_root.parent().expect("the env's folder").to_path_buf();
    let stub = folder.join("stub-julia");
    sot_log::test_exec::write_executable(&stub, STUB);
    let bin = folder.join("bin");
    std::fs::create_dir_all(&bin).expect("an empty PATH entry");
    env.spawn_sotd_with_path_and_env(&bin, &[("SOT_JULIA_BIN", &stub.to_string_lossy())]);
    folder.join("stub-julia.log")
}

#[tokio::test]
async fn a_window_that_redials_mid_eval_still_gets_its_run() {
    let env = Env::new("replredial");
    stub_daemon(&env);
    let mut a = Window::open_as(&env, "window-a").await;
    let ack = a.request(op::REPL_EVAL, json!({ "eval_id": 1, "code": "late-a" })).await;
    assert_eq!(ack["eval_id"], 1, "{ack}");
    drop(a); // the link drops while the eval runs; the same window redials
    let mut again = Window::open_as(&env, "window-a").await;
    while !again.frames.iter().any(|(_, f)| f["kind"] == "done") {
        again.read_one().await;
    }
    assert_eq!(again.under(1), ["stdout late-a", "done"], "the window's entry 1 gets its run: {:?}", again.frames);
    env.kill_daemon_bounded().await;
}

/// The window's Ctrl+C after a redial names its eval by its own number, as its drawer shows it, and reaches the REPL
/// under the daemon's number for the run.
#[tokio::test]
async fn a_window_that_redials_mid_eval_can_still_interrupt_it() {
    let env = Env::new("replredint");
    let log = stub_daemon(&env);
    let mut a = Window::open_as(&env, "window-a").await;
    let ack = a.request(op::REPL_EVAL, json!({ "eval_id": 1, "code": "late-a" })).await;
    assert_eq!(ack["eval_id"], 1, "{ack}");
    // An interrupt reaches only a ready REPL; the stand-in reads the eval as it says it is ready.
    while !a.frames.iter().any(|(_, f)| f["kind"] == "lifecycle" && f["state"] == "ready") {
        a.read_one().await;
    }
    drop(a);
    let mut again = Window::open_as(&env, "window-a").await;
    let answer = again.request(op::REPL_INTERRUPT, json!({ "eval_ids": [1] })).await;
    assert_eq!(answer["interrupted"], true, "{answer}");
    let read: Vec<Value> = std::fs::read_to_string(&log)
        .expect("the stand-in's log")
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let eval = read.iter().find(|v| v["op"] == op::REPL_EVAL).expect("the eval reached the REPL");
    let number = eval["payload"]["eval_id"].as_u64().expect("the eval's number");
    let interrupt = read.iter().find(|v| v["op"] == op::REPL_INTERRUPT).expect("the interrupt reached the REPL");
    assert_eq!(interrupt["payload"]["eval_ids"], json!([number]), "the REPL is named the run by the daemon's number");
    env.kill_daemon_bounded().await;
}
