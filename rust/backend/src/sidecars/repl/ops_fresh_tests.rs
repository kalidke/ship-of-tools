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

/// `ANSWERS` after a start that takes 2 s: a run submitted meanwhile waits in the queue's start.
const SLOW_START: &str = r#"#!/bin/sh
sleep 2
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  eid=$(printf '%s' "$line" | sed -n -e 's/.*"eval_id":\([0-9]*\).*/\1/p')
  printf '{"v":1,"id":%s,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"stdout","text":"ran\\n"}}}\n' "$id" "$eid"
  printf '{"v":1,"id":%s,"kind":"evt","op":"repl.frame","payload":{"eval_id":%s,"frame":{"kind":"done","eval_id":%s,"elapsed_ms":1}}}\n' "$id" "$eid" "$eid"
done
"#;

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

/// A window that redials while its first eval waits for a starting REPL: the hello read then names the run, so the
/// redialed window opens an entry for it, and the run's output and `done` are written to that window.
#[tokio::test]
async fn a_run_accepted_after_a_redials_hello_is_named_to_the_redialed_window() {
    let mut r = row(SLOW_START);
    let window = CallerNumbers::default();
    let eval = json!({ "workspace_id": r.id, "eval_id": 7, "code": "1" });
    let submit = handle_repl_eval(1, eval, &r.session, &r.workspaces, &window);
    // The run is announced (`started`) as soon as it is numbered, and the REPL's queue takes it only after the
    // 2 s start: a hello read at the announcement is a redial's during the start.
    let mut announced = r.bus.resubscribe();
    let hello = async {
        while next_frame(&mut announced).await.1["kind"] != "started" {}
        window.running(&announced)
    };
    let (out, named) = tokio::join!(submit, hello);
    assert_eq!(named, [7], "a hello during the REPL's start names the window's run");
    assert_eq!(out.expect("handler")[0].0.payload["accepted"], true);
    let mut written = Vec::new();
    while !written.iter().any(|(_, kind): &(u64, String)| kind == "done") {
        let (id, frame) = next_frame(&mut r.bus).await;
        let msg = crate::sidecars::repl::ReplFrameMsg { eval_id: id, workspace_id: None, frame, opener: Default::default() };
        if let Some(m) = window.outgoing(msg).filter(|m| m.eval_id != 0) {
            written.push((m.eval_id, m.frame["kind"].as_str().unwrap_or("").to_string()));
        }
    }
    println!("repl-redial-late: the redialed window is written {written:?}");
    assert!(written.iter().all(|(id, _)| *id == 7), "the window's own number: {written:?}");
    assert!(written.iter().any(|(_, kind)| kind == "stdout"), "the output reaches it: {written:?}");
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
