//! Real Julia: an interrupt request sends one ordinary signal, a forced one the burst that stops code with no safepoint,
//! and the REPL keeps what it defined.

use std::time::Duration;

use serde_json::json;

use super::project_tests::{eval, stdout_of, Fixture, BODY};
use crate::sidecars::contract_tests::{isolated, within};

/// Starts eval `id` running `code` and waits until it prints `marker`; returns its reply and frames.
async fn start(
    fixture: &Fixture,
    id: u64,
    code: &str,
    marker: &str,
) -> (
    tokio::sync::oneshot::Receiver<anyhow::Result<serde_json::Value>>,
    crate::sidecars::repl::ExecCollector,
) {
    let (reply, collector) = fixture
        .repl
        .execute("repl.eval", json!({ "code": code, "eval_id": id }))
        .await
        .expect("the REPL must start");
    within(Duration::from_secs(120), "the eval runs", || {
        stdout_of(&collector.lock().expect("collector").frames).contains(marker)
    })
    .await;
    (reply, collector)
}

/// The eval's own error frames name an InterruptException.
fn interrupted(collector: &crate::sidecars::repl::ExecCollector) -> bool {
    collector.lock().expect("collector").frames.iter().any(|f| {
        f["kind"] == "error" && f["message"].as_str().is_some_and(|m| m.contains("InterruptException"))
    })
}

/// One interrupt request sends one ordinary signal, which stops code that has a safepoint (the stock REPL's Ctrl-C).
#[tokio::test]
async fn one_interrupt_stops_a_loop_with_a_safepoint() {
    if !isolated("sidecars::repl::interrupt_tests::one_interrupt_stops_a_loop_with_a_safepoint", BODY) {
        return;
    }
    let fixture = Fixture::new();
    eval(&fixture.repl, 1, "kept = 41").await;
    let code = "println(\"summing\"); let s = 0.0; for i in 1:10^12; s += sin(i); end; s; end";
    let (reply, collector) = start(&fixture, 2, code, "summing").await;
    let answer = fixture.repl.request_if_running("repl.interrupt", json!({})).await;
    let answer = answer.expect("the interrupt must be answered").expect("the REPL is running");
    assert_eq!(answer["interrupted"], true, "{answer}");
    tokio::time::timeout(Duration::from_secs(30), reply).await.expect("the eval answers").ok();
    assert!(interrupted(&collector));
    let after = eval(&fixture.repl, 3, "kept + 1").await;
    assert!(after.iter().any(|f| f["kind"] == "value" && f["text"] == "42"), "{after:?}");
    fixture.finish().await;
}

/// One request on code with no safepoint sends one signal, which does not stop it: the answer says a second Ctrl+C or
/// `--force` forces it, and a forced request stops it, the REPL keeping its state.
#[tokio::test]
async fn a_loop_with_no_safepoint_needs_the_force() {
    if !isolated("sidecars::repl::interrupt_tests::a_loop_with_no_safepoint_needs_the_force", BODY) {
        return;
    }
    let fixture = Fixture::new();
    eval(&fixture.repl, 1, "kept = 41").await;
    let (mut reply, collector) = start(&fixture, 2, "println(\"spinning\"); while true end", "spinning").await;
    let answer = fixture.repl.request_if_running("repl.interrupt", json!({})).await;
    let error = format!("{:#}", answer.expect_err("one ordinary signal does not stop this loop"));
    assert!(error.contains("second Ctrl+C") && error.contains("--force"), "the answer names the force: {error}");
    assert!(error.contains("--fresh"), "and the restart: {error}");
    assert!(tokio::time::timeout(Duration::from_secs(3), &mut reply).await.is_err(), "the loop runs on");
    assert!(!interrupted(&collector));
    let forced = fixture.repl.request_if_running("repl.interrupt", json!({ "force": true })).await;
    let forced = forced.expect("the forced interrupt must be answered").expect("the REPL is running");
    assert_eq!(forced["interrupted"], true, "{forced}");
    tokio::time::timeout(Duration::from_secs(30), reply).await.expect("the eval answers").ok();
    assert!(interrupted(&collector));
    let after = eval(&fixture.repl, 3, "kept + 1").await;
    assert!(after.iter().any(|f| f["kind"] == "value" && f["text"] == "42"), "{after:?}");
    fixture.finish().await;
}

/// A second Ctrl+C a moment after the first forces: the forced request replaces the plain one's escalation, the burst
/// goes and the loop stops, and the forced request is answered.
#[tokio::test]
async fn a_forced_request_a_moment_after_a_plain_one_forces() {
    if !isolated("sidecars::repl::interrupt_tests::a_forced_request_a_moment_after_a_plain_one_forces", BODY) {
        return;
    }
    let fixture = Fixture::new();
    eval(&fixture.repl, 1, "kept = 41").await;
    let (reply, collector) = start(&fixture, 2, "println(\"spinning\"); while true end", "spinning").await;
    let plain = fixture.repl.request_if_running("repl.interrupt", json!({ "eval_ids": [2] }));
    let forced = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        fixture.repl.request_if_running("repl.interrupt", json!({ "eval_ids": [2], "force": true })).await
    };
    let (_plain, forced) = tokio::join!(plain, forced);
    let forced = forced.expect("the forced interrupt must be answered").expect("the REPL is running");
    assert_eq!(forced["interrupted"], true, "{forced}");
    tokio::time::timeout(Duration::from_secs(30), reply).await.expect("the eval answers").ok();
    assert!(interrupted(&collector));
    let after = eval(&fixture.repl, 3, "kept + 1").await;
    assert!(after.iter().any(|f| f["kind"] == "value" && f["text"] == "42"), "{after:?}");
    fixture.finish().await;
}

/// An interrupt that names other evals signals nothing: the running eval was not named.
#[tokio::test]
async fn an_interrupt_naming_another_eval_sends_no_signal() {
    if !isolated("sidecars::repl::interrupt_tests::an_interrupt_naming_another_eval_sends_no_signal", BODY) {
        return;
    }
    let fixture = Fixture::new();
    // A loop with a safepoint, which one signal would stop: only the request's naming keeps the signal from it.
    let code = "println(\"spinning\"); let s = 0.0; for i in 1:10^12; s += sin(i); end; s; end";
    let (mut reply, collector) = start(&fixture, 2, code, "spinning").await;
    let answer = fixture.repl.request_if_running("repl.interrupt", json!({ "eval_ids": [99] })).await;
    assert!(answer.is_err(), "nothing answers an interrupt for an eval that is not running");
    assert!(tokio::time::timeout(Duration::from_secs(5), &mut reply).await.is_err(), "eval 2 was not named");
    assert!(!interrupted(&collector), "no signal reached eval 2");
    fixture.finish().await;
}

/// Native code the signal cannot enter: one request leaves the REPL alive and serving, as the stock REPL's Ctrl-C does.
/// The follow-up `again` must not name anything `long` defines: the interrupt can land before or after any of those
/// definitions, and the follow-up has to be answered with a value either way. Names from `setup` are safe, as an
/// interrupt never undoes them.
async fn one_interrupt_leaves_the_repl_alive(name: &str, setup: &str, long: &str, after_secs: u64, again: &str) {
    if !isolated(name, BODY) {
        return;
    }
    let fixture = Fixture::new();
    eval(&fixture.repl, 1, setup).await;
    let (reply, _collector) = start(&fixture, 2, &format!("println(\"going\"); {long}"), "going").await;
    tokio::time::sleep(Duration::from_secs(after_secs)).await;
    let answer = fixture.repl.request_if_running("repl.interrupt", json!({})).await;
    println!("repl-interrupt: {name} -> {answer:?}");
    tokio::time::timeout(Duration::from_secs(120), reply).await.expect("the eval answers").ok();
    let after = eval(&fixture.repl, 3, again).await;
    assert!(!after.iter().any(|f| f["kind"] == "error"), "the REPL serves the next call: {after:?}");
    assert!(after.iter().any(|f| f["kind"] == "value"), "the next call answers a value: {after:?}");
    fixture.finish().await;
}

#[tokio::test]
async fn one_interrupt_during_a_long_blas_call_leaves_the_repl_alive() {
    one_interrupt_leaves_the_repl_alive(
        "sidecars::repl::interrupt_tests::one_interrupt_during_a_long_blas_call_leaves_the_repl_alive",
        "using LinearAlgebra; BLAS.set_num_threads(4); A = rand(9000, 9000); nothing",
        "B = A * A; nothing",
        0,
        "C = A * A; size(C)",
    )
    .await;
}

#[tokio::test]
async fn one_interrupt_during_a_long_compile_leaves_the_repl_alive() {
    one_interrupt_leaves_the_repl_alive(
        "sidecars::repl::interrupt_tests::one_interrupt_during_a_long_compile_leaves_the_repl_alive",
        "nothing",
        "ex = Expr(:block, [:(x = sin(x) + $i) for i in 1:20000]...); @eval function interrupted_compile(x); $ex; x; end; Base.invokelatest(interrupted_compile, 0.5)",
        0,
        "sin(1.5)",
    )
    .await;
}

#[tokio::test]
async fn one_interrupt_during_a_full_collection_leaves_the_repl_alive() {
    one_interrupt_leaves_the_repl_alive(
        "sidecars::repl::interrupt_tests::one_interrupt_during_a_full_collection_leaves_the_repl_alive",
        "heap = [Ref(i) for i in 1:40_000_000]; nothing",
        "for _ in 1:40; GC.gc(true); end",
        0,
        "GC.gc(true); length(heap)",
    )
    .await;
}
