#![cfg(target_os = "linux")]
//! A Julia child's temporary folder outlives no daemon (Linux, a shell stand-in for Julia's REPL). A daemon's child
//! folders are in a folder of its own in its temporary folder (`TMPDIR`, here a folder of the test's own), which the
//! daemon holds by a lock while it has child folders, and with a display a REPL child's holds the display's cookie and
//! log. A stop (TERM to the guard) and a close remove it at the daemon's terminal. A SIGKILL of the guard runs no code
//! in the daemon and leaves it, and the next daemon's start in the same temporary folder removes it, while a running
//! daemon's stays, whether or not the two daemons share a pid namespace.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use serde_json::json;
use sot_protocol::op;
use std::path::{Path, PathBuf};
use support::{call, connect_and_hello, poll_until, try_connect, Env, BOUND};

/// A `julia` stand-in that says it is ready and then reads its input to its end, with shell builtins alone, so it runs
/// with any `PATH`.
const STUB: &str = "#!/bin/sh\nprintf '{\"v\":1,\"id\":0,\"kind\":\"evt\",\"op\":\"repl.ready\",\"payload\":{}}\\n'\nwhile IFS= read -r line; do :; done\n";

/// Every entry in `tmp`, the daemons' temporary folder, by name.
fn child_folders(tmp: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(tmp)
        .expect("the daemons' temporary folder")
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
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

/// SIGKILLs `env`'s guard, the process this test spawned, so its daemon dies at once (`PR_SET_PDEATHSIG`) and runs no
/// code; returns once the daemon is gone, with the daemon's pid. The daemon is the guard's one child.
async fn kill_the_guard(env: &Env) -> u32 {
    let mut guard = env.daemon.borrow_mut().take().expect("the launched guard");
    let children = std::fs::read_to_string(format!("/proc/{0}/task/{0}/children", guard.id())).expect("the guard's children");
    let daemon: Vec<u32> = children.split_whitespace().map(|pid| pid.parse().expect("a pid")).collect();
    assert_eq!(daemon.len(), 1, "the guard's one child is its daemon: {children:?}");
    guard.kill().expect("SIGKILL to the guard");
    sot_log::test_isolated::wait_within(&mut guard, BOUND);
    let proc_dir = format!("/proc/{}", daemon[0]);
    poll_until(|| async { (!Path::new(&proc_dir).exists()).then_some(()) }, BOUND, "the killed guard's daemon to be gone").await;
    assert!(try_connect(&env.socket_path).await.is_none(), "the killed guard's daemon answers no more");
    daemon[0]
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

/// A close (a lease, then `fe.leaving` close), which ends through `shutdown::exit` and not the signal path, removes the
/// REPL child's folder too.
#[tokio::test]
async fn a_close_removes_its_childrens_folders() {
    let tmp = tempfile::tempdir().expect("the daemons' temporary folder");
    let env = Env::new("childclose");
    let made = daemon_with_a_child(&env, tmp.path(), true).await;
    let me = sot_log::identity::challenge::self_identity().expect("this process's identity");
    let req = sot_protocol::ops::FeLeaseReq { boot: me.boot, pid: me.pid, created: me.created, token: None };
    let lease = sot_protocol::Frame::req(1, op::FE_LEASE, serde_json::to_value(&req).expect("the lease request"));
    let mut conn = support::handoff(&env.socket_path, &lease).await;
    let _ = tokio::time::timeout(BOUND, sot_protocol::codec::read_frame(&mut conn)).await.expect("the lease reply in time").expect("the lease reply");
    let leaving = sot_protocol::Frame::req(2, op::FE_LEAVING, json!({ "intent": "close" }));
    let _ = sot_protocol::codec::write_frame(&mut conn, &leaving, None).await;
    let mut guard = env.daemon.borrow_mut().take().expect("the launched guard");
    let status = sot_log::test_isolated::wait_within(&mut guard, BOUND);
    println!("child-folders: the closed daemon's guard ended: {status:?}; folders left: {:?}", present(tmp.path(), &made));
    assert_eq!(present(tmp.path(), &made), Vec::<String>::new(), "a closed daemon's child folders are gone");
}

#[tokio::test]
async fn a_killed_guards_folders_go_at_the_next_start_and_a_running_daemons_stay() {
    let tmp = tempfile::tempdir().expect("the daemons' temporary folder");
    let running = Env::new("childrun");
    let kept = daemon_with_a_child(&running, tmp.path(), false).await;
    let killed = Env::new("childkill");
    let left = daemon_with_a_child(&killed, tmp.path(), false).await;
    kill_the_guard(&killed).await;
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

/// Whether this account can make a pid namespace here (`unshare` with a user namespace of its own); says why not.
fn pid_namespaces_here() -> bool {
    let probe = sot_log::test_wait::try_run_within(
        std::process::Command::new("unshare").args(["-Upf", "--map-root-user", "--mount-proc", "true"]),
        BOUND,
    );
    let why = match probe {
        Ok(ran) if ran.status.success() => return true,
        Ok(ran) => ran.to_string(),
        Err(why) => why,
    };
    eprintln!("skipped: this account cannot make a pid namespace here (`unshare -Upf`): {why}");
    false
}

/// A `sotd` launcher that starts the daemon in a pid namespace of its own (made with a user namespace, so this account
/// may make it) in which a stand-in process holds pid `taken` before the daemon starts. A SIGKILL of the launched
/// `unshare` ends every process in the namespace (`--kill-child`).
fn in_own_pid_namespace(folder: &Path, taken: u32) -> PathBuf {
    let launcher = folder.join("sotd-in-own-pid-namespace");
    let body = format!(
        "#!/bin/sh\nexec unshare -Upf --map-root-user --mount-proc --kill-child -- /bin/sh -c \
         'echo {} > /proc/sys/kernel/ns_last_pid || exit 1; /bin/sleep 600 & exec \"$0\" \"$@\"' '{}' \"$@\"\n",
        taken - 1,
        support::sotd_program().display()
    );
    sot_log::test_exec::write_executable(&launcher, body);
    launcher
}

/// The pids, in their own pid namespace, of the children of the first process of the namespace `launched` (its
/// `unshare`) made.
fn namespace_pids(launched: u32) -> Vec<u32> {
    let children = |pid: u32| -> Vec<u32> {
        let listed = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
        listed.split_whitespace().filter_map(|pid| pid.parse().ok()).collect()
    };
    children(launched)
        .into_iter()
        .flat_map(children)
        .filter_map(|pid| {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
            status.lines().find_map(|line| line.strip_prefix("NSpid:"))?.split_whitespace().last()?.parse().ok()
        })
        .collect()
}

/// Two daemons that share a temporary folder but not a pid namespace, as daemons on two computers sharing one over NFS
/// do: a start in a pid namespace of its own removes a crashed daemon's child folders though the crashed daemon's pid
/// is running there (held by another process, as a reused pid is), and never a running daemon's, though no pid of the
/// running daemon is running there. Skipped, saying why, where this account cannot make the namespaces.
#[tokio::test]
async fn a_start_in_another_pid_namespace_removes_a_crashed_daemons_folders_and_never_a_running_ones() {
    if !pid_namespaces_here() {
        return;
    }
    let tmp = tempfile::tempdir().expect("the daemons' temporary folder");
    let running = Env::new("childnsrun");
    let kept = daemon_with_a_child(&running, tmp.path(), false).await;
    let crashed = Env::new("childnscrash");
    let left = daemon_with_a_child(&crashed, tmp.path(), false).await;
    let taken = kill_the_guard(&crashed).await;

    let sweeper = Env::new("childnssweep");
    let launcher = in_own_pid_namespace(sweeper.daemon_project_root.parent().expect("the env's folder"), taken);
    let tmp_var = tmp.path().to_string_lossy();
    sweeper.spawn_sotd_at(&launcher, &[("TMPDIR", &*tmp_var), ("RUST_LOG", "info")]);
    let log = sweeper.state_root.join("sot").join("sotd.log");
    poll_until(
        || async { std::fs::read_to_string(&log).ok()?.contains("temporary folders of ended daemons removed").then_some(()) },
        BOUND,
        "the sweeping daemon's start to remove what it removes",
    )
    .await;
    let mut launched = sweeper.daemon.borrow_mut().take().expect("the launched unshare");
    let pids = namespace_pids(launched.id());
    let after = (present(tmp.path(), &kept), present(tmp.path(), &left));
    println!("child-folders: the running daemon's {kept:?} and the crashed daemon's {left:?} (pid {taken}); after a start in a pid namespace holding pids {pids:?}, present: {after:?}");
    launched.kill().expect("SIGKILL to the launched unshare");
    sot_log::test_isolated::wait_within(&mut launched, BOUND);
    assert!(pids.contains(&taken), "premise: pid {taken} runs in the sweeping daemon's pid namespace: {pids:?}");
    assert_eq!(after, (kept.clone(), Vec::new()), "a running daemon's child folders stay and a crashed one's go");
    running.kill_daemon_bounded().await;
    assert_eq!(present(tmp.path(), &kept), Vec::<String>::new(), "and the running daemon's go at its stop");
}
