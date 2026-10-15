#![cfg(target_os = "linux")]
//! A Julia child's temporary folder outlives no daemon (Linux, a shell stand-in for Julia's REPL). A REPL child's folder
//! is `sot-julia-<daemon pid>-<random>` in the daemon's temporary folder (`TMPDIR`, here a folder of the test's own),
//! and with a display it holds the display's cookie and log. A stop (TERM to the guard) removes it at the daemon's
//! terminal. A SIGKILL of the guard runs no code in the daemon and leaves it, and the next daemon's start in the same
//! temporary folder removes it, while a running daemon's folder stays.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use serde_json::json;
use sot_protocol::op;
use std::path::Path;
use support::{call, connect_and_hello, poll_until, try_connect, Env, BOUND};

/// A `julia` stand-in that says it is ready and then reads its input to its end, with shell builtins alone, so it runs
/// with any `PATH`.
const STUB: &str = "#!/bin/sh\nprintf '{\"v\":1,\"id\":0,\"kind\":\"evt\",\"op\":\"repl.ready\",\"payload\":{}}\\n'\nwhile IFS= read -r line; do :; done\n";

/// The child folders in `tmp`, by name.
fn child_folders(tmp: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(tmp)
        .expect("the daemons' temporary folder")
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("sot-julia-"))
        .collect();
    names.sort();
    names
}

/// Starts `env`'s daemon with `tmp` as its temporary folder and has it start its default row's REPL child; returns the
/// child folders the start made. With `display` the daemon's `PATH` keeps the system's folders, so the child gets its own
/// X display; without, `PATH` holds only an empty folder and the child gets none, so no X server outlives a SIGKILL of
/// the guard (an Xvfb never ends on its own).
async fn daemon_with_a_child(env: &Env, tmp: &Path, display: bool) -> Vec<String> {
    let folder = env.daemon_project_root.parent().expect("the env's folder").to_path_buf();
    let stub = folder.join("stub-julia");
    sot_log::test_exec::write_executable(&stub, STUB);
    let bin = folder.join("bin");
    std::fs::create_dir_all(&bin).expect("an empty PATH entry");
    let (stub, tmp_var, bin_only) = (stub.to_string_lossy(), tmp.to_string_lossy(), bin.to_string_lossy());
    let mut vars = vec![("SOT_JULIA_BIN", &*stub), ("TMPDIR", &*tmp_var)];
    if !display {
        vars.push(("PATH", &*bin_only));
    }
    let before = child_folders(tmp);
    env.spawn_sotd_with_path_and_env(&bin, &vars);
    let (mut conn, id) = connect_and_hello(&env.socket_path).await;
    let ack = call(&mut conn, id, op::REPL_EVAL, json!({ "eval_id": 1, "code": "1" })).await;
    assert_eq!(ack.payload["accepted"], true, "the eval is accepted: {:?}", ack.payload);
    let made: Vec<String> = child_folders(tmp).into_iter().filter(|name| !before.contains(name)).collect();
    assert!(!made.is_empty(), "the REPL child's start makes its folder in {}", tmp.display());
    made
}

/// The names in `names` still present in `tmp`.
fn present(tmp: &Path, names: &[String]) -> Vec<String> {
    names.iter().filter(|name| tmp.join(name).exists()).cloned().collect()
}

#[tokio::test]
async fn a_stop_removes_its_childrens_folders() {
    let tmp = tempfile::tempdir().expect("the daemons' temporary folder");
    let env = Env::new("childstop");
    let made = daemon_with_a_child(&env, tmp.path(), true).await;
    let held: Vec<_> = std::fs::read_dir(tmp.path().join(&made[0])).expect("the child's folder").flatten().map(|e| e.file_name()).collect();
    println!("child-folders: the REPL child's folder {made:?} holds {held:?}");
    env.kill_daemon_bounded().await; // TERM to the guard: the daemon's terminal (`support::end_daemon`)
    assert_eq!(present(tmp.path(), &made), Vec::<String>::new(), "a stopped daemon's child folders are gone");
}

#[tokio::test]
async fn a_killed_guards_folders_go_at_the_next_start_and_a_running_daemons_stay() {
    let tmp = tempfile::tempdir().expect("the daemons' temporary folder");
    let running = Env::new("childrun");
    let kept = daemon_with_a_child(&running, tmp.path(), false).await;
    let killed = Env::new("childkill");
    let left = daemon_with_a_child(&killed, tmp.path(), false).await;

    // A SIGKILL of the guard, the process this test spawned: the daemon dies at once (`PR_SET_PDEATHSIG`) and runs no
    // code. The daemon is the guard's one child; it is gone once its pid is.
    let mut guard = killed.daemon.borrow_mut().take().expect("the launched guard");
    let children = std::fs::read_to_string(format!("/proc/{0}/task/{0}/children", guard.id())).expect("the guard's children");
    let daemon: Vec<&str> = children.split_whitespace().collect();
    assert_eq!(daemon.len(), 1, "the guard's one child is its daemon: {children:?}");
    guard.kill().expect("SIGKILL to the guard");
    sot_log::test_isolated::wait_within(&mut guard, BOUND);
    let proc_dir = format!("/proc/{}", daemon[0]);
    poll_until(|| async { (!Path::new(&proc_dir).exists()).then_some(()) }, BOUND, "the killed guard's daemon to be gone").await;
    assert!(try_connect(&killed.socket_path).await.is_none(), "the killed guard's daemon answers no more");
    assert_eq!(present(tmp.path(), &left), left, "a SIGKILL of the guard runs no code in the daemon: its folders stay");

    let next = Env::new("childnext");
    let tmp_var = tmp.path().to_string_lossy();
    next.spawn_sotd_with_env(&[("TMPDIR", &*tmp_var)]);
    connect_and_hello(&next.socket_path).await;
    poll_until(
        || async { present(tmp.path(), &left).is_empty().then_some(()) },
        BOUND,
        "the next start to remove the ended daemon's child folders",
    )
    .await;
    assert_eq!(present(tmp.path(), &kept), kept, "a running daemon's child folders stay");
    next.kill_daemon_bounded().await;
    running.kill_daemon_bounded().await;
    assert_eq!(present(tmp.path(), &kept), Vec::<String>::new(), "and go at its stop");
}
