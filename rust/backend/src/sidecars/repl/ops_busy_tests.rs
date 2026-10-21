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

/// An eval that goes on through the one ordinary signal (the stand-in ignores it) is answered by the daemon, not left to
/// the bound: the error says a second Ctrl+C or `--force` forces it and `--fresh` restarts. A forced interrupt that
/// is gone through says forcing did not stop it either.
#[tokio::test]
async fn an_interrupt_the_signal_does_not_stop_says_so() {
    let row = deaf_row().await;
    let conn = CallerNumbers::default();
    let t0 = std::time::Instant::now();
    let plain = handle_repl_interrupt(1, json!({ "workspace_id": row.id }), &row.session, &row.workspaces, &conn);
    let plain = bounded("an interrupt", plain).await.expect("handler");
    let waited = t0.elapsed();
    assert_eq!(code_of(&plain), "repl_interrupt_failed");
    let error = plain[0].0.payload["error"].as_str().unwrap_or("").to_string();
    assert!(error.contains("second Ctrl+C") && error.contains("--force") && error.contains("--fresh"), "{error}");
    assert!(waited < Duration::from_secs(6), "answered by the daemon, not at the bound: {waited:?}");
    let forced = json!({ "workspace_id": row.id, "force": true });
    let forced = handle_repl_interrupt(2, forced, &row.session, &row.workspaces, &conn);
    let forced = bounded("a forced interrupt", forced).await.expect("handler");
    let error = forced[0].0.payload["error"].as_str().unwrap_or("").to_string();
    assert!(error.contains("forced") && error.contains("--fresh") && !error.contains("second Ctrl+C"), "{error}");
    println!("repl-busy: an interrupt the signal does not stop -> {error} after {waited:?} (first)");
}

/// An interrupt no one answers and no eval is in flight to signal ends at its bound.
#[tokio::test]
async fn an_unanswered_interrupt_ends_at_its_bound() {
    let row = row_running(ANSWERED_FIRST).await;
    let done = row._tmp.path().join("julia.done");
    within(Duration::from_secs(10), "the eval ended", || done.exists()).await;
    let t0 = std::time::Instant::now();
    let conn = CallerNumbers::default();
    let interrupt = handle_repl_interrupt(1, json!({ "workspace_id": row.id }), &row.session, &row.workspaces, &conn);
    let unanswered = bounded("an interrupt", interrupt).await.expect("handler");
    let waited = t0.elapsed();
    assert_eq!(code_of(&unanswered), "repl_interrupt_failed");
    let error = unanswered[0].0.payload["error"].as_str().unwrap_or("").to_string();
    assert!(error.contains("--fresh"), "the error names the restart: {error}");
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
