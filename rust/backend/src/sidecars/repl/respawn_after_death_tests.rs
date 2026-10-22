use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use crate::sidecars::contract_tests::pin_env;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory, unique per (process, call).
fn scratch_dir(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!(
        "sot-repl-respawn-{}-{}-{name}",
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A `julia` stand-in that appends its argv to `log` and then runs `body`.
fn write_stub(path: &Path, log: &Path, body: &str) {
    sot_platform::test_exec::write_executable(
        path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$* LP=$JULIA_LOAD_PATH\" >> '{}'\n{body}\n",
            log.display()
        ),
    );
}

fn spawn_argv(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A resource root shaped the way `paths::resource_dir` expects, i.e. with
/// a real `julia/repl` inside it.
fn resource_root(dir: &Path, name: &str) -> PathBuf {
    let root = dir.join(name);
    std::fs::create_dir_all(root.join("julia").join("repl")).unwrap();
    root
}

/// Submit and wait with a deadline the way `handle_repl_execute` does. `Err`
/// means the submission never settled — the failure this module is about.
async fn settle(
    repl: &Repl,
    eval_id: u64,
) -> std::result::Result<(), tokio::time::error::Elapsed> {
    let (reply_rx, _collector) = repl
        .execute("repl.eval", serde_json::json!({ "eval_id": eval_id }))
        .await
        .expect("submit must not error once a child spawned");
    tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx)
        .await
        .map(|_| ())
}

/// A child that dies the instant it is spawned: every later submit gets a
/// FRESH child (one spawn each), and every one of them settles.
#[tokio::test]
async fn submit_after_instant_child_death_respawns() {
    let dir = scratch_dir("instant-death");
    let root = resource_root(&dir, "resources");
    let log = dir.join("argv.log");
    let julia = dir.join("julia");
    write_stub(&julia, &log, "exit 3");
    let _pin = pin_env(&julia, &root);

    let (frame_tx, _frame_rx) = broadcast::channel(64);
    let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), sot_containment::process());

    for attempt in 1..=3u64 {
        settle(&repl, attempt)
            .await
            .unwrap_or_else(|_| panic!("attempt {attempt} never settled"));
    }
    assert_eq!(
        spawn_argv(&log).len(),
        3,
        "each submit after a death must spawn its own child"
    );
    assert_eq!(repl.state(), ReplLifecycle::Dead);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A launcher that exits leaving a grandchild holding the inherited pipes:
/// stdout never reaches EOF, so before the `child.wait()` branch the
/// supervisor outlived its child with a submit channel that still reported
/// open — and the submission sat there unread, forever.
#[tokio::test]
async fn submit_survives_a_launcher_that_leaves_a_grandchild() {
    let dir = scratch_dir("grandchild");
    let root = resource_root(&dir, "resources");
    let log = dir.join("argv.log");
    let julia = dir.join("julia");
    // `exec 3<&0` hands the grandchild a DUP of the stdin pipe (dash
    // otherwise redirects a background command's stdin from /dev/null),
    // so both pipe ends outlive the launcher: writes never break and
    // stdout never reaches EOF. Only the child's own exit says it is gone.
    write_stub(&julia, &log, "exec 3<&0\nsleep 30 &\nexit 0");
    let _pin = pin_env(&julia, &root);

    let (frame_tx, _frame_rx) = broadcast::channel(64);
    let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), sot_containment::process());

    settle(&repl, 1)
        .await
        .expect("a launcher's exit must end the supervisor, not strand the submit");
    settle(&repl, 2).await.expect("the retry must settle too");
    assert_eq!(spawn_argv(&log).len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A spawn that cannot happen at all is reported to the caller AT ONCE, on
/// every attempt, and never claims a child exists.
#[tokio::test]
async fn spawn_failure_is_returned_immediately() {
    let dir = scratch_dir("no-julia");
    let root = resource_root(&dir, "resources");
    let missing = dir.join("julia-that-is-not-there");
    let _pin = pin_env(&missing, &root);

    let (frame_tx, _frame_rx) = broadcast::channel(64);
    let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), sot_containment::process());

    for attempt in 1..=2u64 {
        let submitted = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            repl.execute("repl.eval", serde_json::json!({ "eval_id": attempt })),
        )
        .await
        .unwrap_or_else(|_| panic!("attempt {attempt}: a failed spawn must not wait"));
        let err = submitted.err().expect("a failed spawn must be an error");
        assert!(
            format!("{err:#}").contains(&missing.display().to_string()),
            "the error must name what could not be spawned: {err:#}"
        );
        assert_eq!(
            repl.state(),
            ReplLifecycle::NotStarted,
            "a spawn that never happened must not report a child"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The REPL project is resolved on EVERY spawn attempt. This is the field
/// sequence: a box ships without `julia/repl`, the operator fixes it, and
/// the very next submit must use the fixed path — the daemon is not
/// restarted, and a path cached on the handle would keep it broken forever.
#[tokio::test]
async fn repl_project_is_resolved_at_every_spawn() {
    let dir = scratch_dir("reresolve");
    let broken = dir.join("broken"); // exists, but holds no julia/repl
    std::fs::create_dir_all(&broken).unwrap();
    let fixed = resource_root(&dir, "fixed");
    let log = dir.join("argv.log");
    let julia = dir.join("julia");
    write_stub(&julia, &log, "exit 3");
    let _pin = pin_env(&julia, &broken);

    let (frame_tx, _frame_rx) = broadcast::channel(64);
    let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), sot_containment::process());

    // First attempt: the root has no `julia/repl`, so resolution falls
    // through to this checkout's own copy — NOT the fixed root.
    settle(&repl, 1).await.expect("attempt 1 never settled");
    let first = spawn_argv(&log);
    assert_eq!(first.len(), 1);
    assert!(
        !first[0].contains(&fixed.display().to_string()),
        "the fixed root did not exist yet: {first:?}"
    );

    // The operator fixes the install. No restart, no new handle.
    std::env::set_var("SOT_RESOURCE_ROOT", &fixed);
    settle(&repl, 2).await.expect("attempt 2 never settled");
    let second = spawn_argv(&log);
    assert_eq!(second.len(), 2);
    assert!(
        second[1].contains(&fixed.join("julia").join("repl").display().to_string()),
        "the next spawn must use the fixed path: {second:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The guard on the new child-exit branch: a child that ANSWERS and then
/// exits in the same breath must still deliver its answer. The exit branch
/// is polled last, so the res line it already wrote is routed first — the
/// caller gets the reply, not the death's error (`ReplEnded::Died`).
#[tokio::test]
async fn a_child_that_answers_then_exits_still_delivers_its_answer() {
    let dir = scratch_dir("answer-then-exit");
    let root = resource_root(&dir, "resources");
    let log = dir.join("argv.log");
    let julia = dir.join("julia");
    write_stub(
        &julia,
        &log,
        "read -r line || exit 1\n             printf '%s\\n' '{\"v\":1,\"id\":1,\"kind\":\"res\",\"op\":\"repl.eval\",\"payload\":{\"answered\":true}}'\n             exit 0",
    );
    let _pin = pin_env(&julia, &root);

    let (frame_tx, _frame_rx) = broadcast::channel(64);
    let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), sot_containment::process());

    let (reply_rx, _collector) = repl
        .execute("repl.eval", serde_json::json!({ "eval_id": 1 }))
        .await
        .expect("submit");
    let payload = tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx)
        .await
        .expect("the reply must arrive")
        .expect("the supervisor must not drop the reply channel")
        .expect("the res must not be replaced by a death error");
    assert_eq!(payload.get("answered").and_then(Value::as_bool), Some(true));
    let _ = std::fs::remove_dir_all(&dir);
}
