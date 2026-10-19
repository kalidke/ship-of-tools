//! Real Julia: an interrupt stops an eval that never yields, through the daemon's signal (`Escalation`), and the REPL
//! keeps what it defined.

use std::time::{Duration, Instant};

use serde_json::json;

use super::project_tests::{eval, stdout_of, Fixture, BODY};
use crate::sidecars::contract_tests::{isolated, within};

/// Code with no safepoint holds the shim's one thread, so the shim cannot read the interrupt: the daemon's signal
/// stops it. The interrupt is answered `interrupted: true` well inside `REPL_INTERRUPT_WAIT`, the eval answers with
/// its InterruptException, and the next eval sees what the first defined.
#[tokio::test]
async fn an_interrupt_stops_an_eval_that_never_yields() {
    if !isolated("sidecars::repl::interrupt_tests::an_interrupt_stops_an_eval_that_never_yields", BODY) {
        return;
    }
    let fixture = Fixture::new();
    eval(&fixture.repl, 1, "kept = 41").await;
    let (reply, collector) = fixture
        .repl
        .execute("repl.eval", json!({ "code": "println(\"spinning\"); while true end", "eval_id": 2 }))
        .await
        .expect("the REPL must start");
    within(Duration::from_secs(60), "the loop runs", || {
        stdout_of(&collector.lock().expect("collector").frames).contains("spinning")
    })
    .await;
    let asked = Instant::now();
    let answer = fixture.repl.request_if_running("repl.interrupt", json!({})).await;
    let waited = asked.elapsed();
    let answer = answer.expect("the interrupt must be answered").expect("the REPL is running");
    println!("repl-interrupt: a loop with no safepoint -> {answer} after {waited:?}");
    assert_eq!(answer["interrupted"], true, "the interrupt stopped the eval: {answer}");
    assert!(waited < Duration::from_secs(8), "answered well inside REPL_INTERRUPT_WAIT: {waited:?}");
    tokio::time::timeout(Duration::from_secs(30), reply)
        .await
        .expect("the eval answers")
        .expect("the supervisor keeps the reply")
        .expect("the eval's answer");
    let frames = collector.lock().expect("collector").frames.clone();
    assert!(
        frames.iter().any(|f| f["kind"] == "error"
            && f["message"].as_str().is_some_and(|m| m.contains("InterruptException"))),
        "the eval answers its InterruptException: {frames:?}"
    );
    let after = eval(&fixture.repl, 3, "kept + 1").await;
    assert!(
        after.iter().any(|f| f["kind"] == "value" && f["text"] == "42"),
        "the REPL keeps its state: {after:?}"
    );
    fixture.finish().await;
}
