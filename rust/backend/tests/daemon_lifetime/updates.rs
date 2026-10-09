//! The update outcomes on real daemons: `update.apply` against a pointer armed with the real updater, the automatic update
//! through the real stage, prepare and arm (an upstream repository, a release folder and an install-shaped prefix built
//! from real programs), the order of a close and an update, the cases an update must leave the daemon serving, and the
//! children an update starts ending with the daemon. Each status is read at the launched process (the guard exits as the
//! daemon did). Orderings the product leaves to chance are held by the daemon's held points (`SOT_TEST_GATES`, feature
//! `daemon-lifetime-faults`), which a case opens once it has seen what it wants to see. The capsule a case starts is
//! outside the daemon's lifetime and stays alive through an update restart.

use crate::fixture_owner::Fixture;
use crate::guard::{
    close_by_lease, lease, leave_with_close, start_spinning_at, start_spinning_with, Run,
};
use crate::routes::{all_ended, ready_row, supervisor_in};
use crate::support::{call, connect_and_hello, Env};
use crate::update_fixture;
use crate::SERIAL;
use sot_protocol::op;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The variables of an update case's daemon: a build the updater takes for a release, the update root and the folder of
/// held points under `case`.
fn vars(case: &Path, mode: &str, release_build: bool) -> Vec<(String, String)> {
    std::fs::create_dir_all(case.join("gates")).expect("the folder of held points");
    let mut vars = vec![
        ("SOT_UPDATE_MODE".to_string(), mode.to_string()),
        (
            "SOT_UPDATE_ROOT".to_string(),
            case.join("updates").display().to_string(),
        ),
        (
            "SOT_TEST_GATES".to_string(),
            case.join("gates").display().to_string(),
        ),
    ];
    if release_build {
        vars.push(("SOT_TEST_RELEASE_BUILD".to_string(), "1".to_string()));
    }
    vars
}

fn refs(vars: &[(String, String)]) -> Vec<(&str, &str)> {
    vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

/// Engage the held point `name` of the case: the daemon waits there until the case opens it.
fn engage(case: &Path, name: &str) {
    std::fs::write(case.join("gates").join(format!("{name}.hold")), b"hold")
        .expect("engage a held point");
}

/// Open the held point `name` of the case.
fn open(case: &Path, name: &str) {
    std::fs::write(case.join("gates").join(name), b"go").expect("open a held point");
}

/// Whether the daemon's log says `needle` within `bound`.
async fn log_says(run: &Run, needle: &str, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        if run.said().contains(needle) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Arm a release in `updates` with the real updater, as the pipeline's last step does, and return its tag.
async fn arm(updates: &Path) -> String {
    let target =
        sot_updater::platform::this_platform().expect("the test host is in the release matrix");
    let version = update_fixture::VERSION;
    let id = sot_updater::ReleaseIdentity {
        repo: "example/project".into(),
        tag: format!("v{version}"),
        version: version.into(),
        target: target.into(),
        asset: sot_updater::platform::platform_asset(version).expect("an asset for this platform"),
        asset_sha256: "0".repeat(64),
    };
    std::fs::create_dir_all(updates).expect("the updates root");
    let armed = sot_updater::pending::arm(updates, &id, &updates.join("checkout"), &"0".repeat(40))
        .await
        .expect("arm the release");
    assert!(armed, "nothing newer was armed");
    id.tag
}

/// A ready capsule row, its supervisor watched; the row's state folder.
async fn row_with_supervisor(run: &Run, fx: &mut Fixture, label: &str) -> (usize, PathBuf) {
    let (mut conn, mut next_id) = connect_and_hello(&run.env.socket_path).await;
    let (_, state_dir) = ready_row(&run.env, &mut conn, &mut next_id, label).await;
    drop(conn);
    let (pid, created) = supervisor_in(&run.env, &state_dir)
        .await
        .expect("the capsule's supervisor answers");
    let supervisor = fx
        .watch(pid, Some(created), "the capsule's supervisor")
        .expect("an identity for the reported supervisor");
    (supervisor, state_dir)
}

fn alive(fx: &Fixture, index: usize) -> bool {
    !fx.identity(index).exited(Duration::from_secs(1))
}

/// A status of the launched process as the code it exited with, or the signal that ended it.
fn describe(status: Option<std::process::ExitStatus>) -> String {
    match status {
        None => "still running".to_string(),
        Some(s) => match (s.code(), s.signal()) {
            (Some(code), _) => format!("exit {code}"),
            (_, Some(signal)) => format!("signal {signal}"),
            _ => format!("{s:?}"),
        },
    }
}

/// An update asked for by `update.apply` against an armed pointer exits 75: the ephemeral tree of a REPL ends with the daemon
/// and the capsule stays.
#[tokio::test]
#[ignore = "needs a Julia 1.12 (SOT_JULIA_BIN, else julia); run by the harness job with --ignored"]
async fn update_apply_exits_75_ends_the_trees_and_leaves_the_capsule() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("update_apply_open_restart_75");
    let case = tempfile::tempdir().unwrap();
    let v = vars(case.path(), "notify", true);
    let mut spinning = start_spinning_with("updapply", &mut fx, false, &refs(&v)).await;
    let (pid, created) = supervisor_in(&spinning.run.env, &spinning.state_dir)
        .await
        .expect("the capsule's supervisor answers");
    let supervisor = fx
        .watch(pid, Some(created), "the capsule's supervisor")
        .expect("an identity for the reported supervisor");
    spinning.task.abort();
    let tag = arm(&case.path().join("updates")).await;
    let (mut conn, id) = connect_and_hello(&spinning.run.env.socket_path).await;
    let applied = call(&mut conn, id, op::UPDATE_APPLY, serde_json::json!({})).await;
    drop(conn);
    fx.save("applied", &applied.payload);
    let status = spinning.run.status_within(Duration::from_secs(60)).await;
    fx.save("status", describe(status));
    fx.save(
        "tree_ended",
        all_ended(&fx, &spinning.ids, Duration::from_secs(10)),
    );
    fx.save("capsule_alive", alive(&fx, supervisor));
    let said = spinning.run.said();
    // The capsule stays by design: the window's close, through a successor, ends it.
    spinning.run.end_capsules().await;
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    let applied = fx.saved("applied").unwrap().to_string();
    assert!(
        applied.contains("\"ok\":true") && applied.contains(&tag),
        "update.apply: {applied}"
    );
    assert_eq!(fx.saved("status"), Some("exit 75"), "{said}");
    assert_eq!(
        fx.saved("tree_ended"),
        Some("true"),
        "the REPL's tree outlived the update restart"
    );
    assert_eq!(
        fx.saved("capsule_alive"),
        Some("true"),
        "the update restart took the capsule with it"
    );
}

/// A close that began first keeps its own exit: the update's decision finds the lease past `Open` and exits nothing.
#[tokio::test]
async fn a_close_that_began_first_keeps_its_exit_over_an_update() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("close_commits_before_update");
    let case = tempfile::tempdir().unwrap();
    let v = vars(case.path(), "notify", true);
    let mut run = Run::boot(Env::new("updclose"), &refs(&v), false, false).await;
    arm(&case.path().join("updates")).await;
    engage(case.path(), "update-go");
    engage(case.path(), "close-go");
    let (mut conn, id) = connect_and_hello(&run.env.socket_path).await;
    let applied = call(&mut conn, id, op::UPDATE_APPLY, serde_json::json!({})).await;
    drop(conn);
    assert_eq!(applied.payload["ok"], true, "{:?}", applied.payload);
    // The update waits at its held point; the close begins and is held in its own.
    close_by_lease(&run.env).await;
    fx.save(
        "close_began",
        log_says(
            &run,
            "shutting down: ending this computer's sessions",
            Duration::from_secs(30),
        )
        .await,
    );
    open(case.path(), "update-go");
    fx.save(
        "update_skipped",
        log_says(&run, "update exit skipped", Duration::from_secs(30)).await,
    );
    open(case.path(), "close-go");
    let status = run.status_within(Duration::from_secs(60)).await;
    fx.save("status", describe(status));
    let said = run.said();
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_eq!(fx.saved("close_began"), Some("true"), "{said}");
    assert_eq!(
        fx.saved("update_skipped"),
        Some("true"),
        "the update was not skipped:\n{said}"
    );
    assert_eq!(
        fx.saved("status"),
        Some("exit 0"),
        "the close's exit did not stand:\n{said}"
    );
    assert!(
        !said.contains("update committed"),
        "an update committed during the close:\n{said}"
    );
}

/// An update committed first stands: a close the window sends afterwards does not begin, and the daemon exits 75.
#[tokio::test]
async fn an_update_committed_first_is_not_undone_by_a_later_close() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("update_commits_before_close");
    let case = tempfile::tempdir().unwrap();
    let v = vars(case.path(), "notify", true);
    let mut run = Run::boot(Env::new("updcommit"), &refs(&v), false, false).await;
    // A window holds a lease from before the update.
    let mut held = lease(&run.env).await;
    arm(&case.path().join("updates")).await;
    engage(case.path(), "update-committed");
    let (mut conn, id) = connect_and_hello(&run.env.socket_path).await;
    let applied = call(&mut conn, id, op::UPDATE_APPLY, serde_json::json!({})).await;
    drop(conn);
    assert_eq!(applied.payload["ok"], true, "{:?}", applied.payload);
    fx.save(
        "committed",
        log_says(
            &run,
            "update committed: exiting 75",
            Duration::from_secs(30),
        )
        .await,
    );
    // The exit is held after the commit; the window now closes.
    leave_with_close(&mut held).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    fx.save(
        "close_began",
        run.said()
            .contains("shutting down: ending this computer's sessions"),
    );
    open(case.path(), "update-committed");
    let status = run.status_within(Duration::from_secs(60)).await;
    fx.save("status", describe(status));
    let said = run.said();
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_eq!(fx.saved("committed"), Some("true"), "{said}");
    assert_eq!(
        fx.saved("close_began"),
        Some("false"),
        "a close began after the update was committed:\n{said}"
    );
    assert_eq!(fx.saved("status"), Some("exit 75"), "{said}");
}

/// An update the daemon may not take leaves it serving: a build that is not a release refuses, and a release build with
/// nothing armed refuses; both answer, neither exits, and the capsule stays.
#[tokio::test]
async fn an_update_the_daemon_may_not_take_leaves_it_serving() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("update_ineligible");
    for (release_build, said_so) in [(false, "disabled: dev build"), (true, "nothing armed")] {
        let case = tempfile::tempdir().unwrap();
        let v = vars(case.path(), "notify", release_build);
        let mut run = Run::boot(Env::new("updno"), &refs(&v), false, false).await;
        let (supervisor, _) = row_with_supervisor(&run, &mut fx, "kept").await;
        let (mut conn, id) = connect_and_hello(&run.env.socket_path).await;
        let applied = call(&mut conn, id, op::UPDATE_APPLY, serde_json::json!({})).await;
        fx.save("ok", applied.payload["ok"].to_string());
        fx.save(
            "status",
            applied.payload["status"].as_str().unwrap_or_default(),
        );
        // Well past the apply's own delay: nothing exits.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let serving = tokio::time::timeout(
            Duration::from_secs(5),
            connect_and_hello(&run.env.socket_path),
        )
        .await
        .is_ok();
        fx.save("serving", serving);
        fx.save(
            "still_running",
            run.status_within(Duration::ZERO).await.is_none(),
        );
        fx.save("capsule_alive", alive(&fx, supervisor));
        drop(conn);
        // The close ends the row; a daemon that exited is replaced by a successor that closes: the assertions say why.
        let closed = run.end_capsules().await;
        let said = run.said();
        let cleanup = fx.cleanup();
        assert!(cleanup.complete(), "{cleanup:?}");
        assert_eq!(
            fx.saved("ok"),
            Some("false"),
            "release build {release_build}"
        );
        assert_eq!(
            fx.saved("status"),
            Some(said_so),
            "release build {release_build}"
        );
        assert_eq!(
            fx.saved("serving"),
            Some("true"),
            "release build {release_build}: the daemon stopped serving:\n{said}"
        );
        assert_eq!(
            fx.saved("still_running"),
            Some("true"),
            "release build {release_build}: the daemon exited:\n{said}"
        );
        assert_eq!(
            fx.saved("capsule_alive"),
            Some("true"),
            "release build {release_build}"
        );
        assert_eq!(
            describe(closed),
            "exit 0",
            "release build {release_build}:\n{said}"
        );
    }
}

/// A stub program for the daemon's PATH that starts a tree and waits: its own pid and its sleeper's go to files in `case`.
fn hanging_stub(case: &Path, program: &str) -> PathBuf {
    let stubs = case.join(format!("{program}-stub"));
    std::fs::create_dir_all(&stubs).unwrap();
    sot_log::test_exec::write_executable(
        &stubs.join(program),
        format!(
            "#!/bin/sh\necho $$ > '{0}/{program}.pid'\n(trap '' HUP TERM; exec sleep 100) &\necho $! > '{0}/{program}.sleeper'\nwait\n",
            case.display()
        ),
    );
    stubs
}

fn path_with(first: &Path) -> String {
    format!(
        "{}:{}",
        first.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// The pids a stub wrote: the stub itself and its sleeper, watched once both are there.
async fn watch_stub(fx: &mut Fixture, case: &Path, program: &str) -> Vec<usize> {
    let read = |name: &str| {
        std::fs::read_to_string(case.join(format!("{program}.{name}")))
            .ok()
            .and_then(|t| t.trim().parse::<i32>().ok())
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let (Some(stub), Some(sleeper)) = (read("pid"), read("sleeper")) {
            return vec![
                fx.watch(stub, None, &format!("the {program} stub"))
                    .expect("an identity for the stub"),
                fx.watch(sleeper, None, &format!("the {program} stub's sleeper"))
                    .expect("an identity for the sleeper"),
            ];
        }
        assert!(
            Instant::now() < deadline,
            "the {program} stub never started"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The daemon is ended (SIGKILL, which it sends itself) while an updater command runs: the command and what it started end
/// with it, and the capsule stays.
async fn update_command_ends_with_the_daemon(program: &str, automatic: bool, tag: &str) {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new(&format!("update_{program}_raw_death"));
    let case = tempfile::tempdir().unwrap();
    let stubs = hanging_stub(case.path(), program);
    let mut v = vars(case.path(), if automatic { "auto" } else { "notify" }, true);
    v.push(("PATH".to_string(), path_with(&stubs)));
    let release = automatic.then(|| update_fixture::build(&case.path().join("release-fixture")));
    if let Some(release) = &release {
        v.push(("SOT_UPDATE_FETCHER".to_string(), release.fetcher.clone()));
    }
    let run = match &release {
        Some(release) => Run::boot_at(Env::new(tag), &release.daemon, &refs(&v)).await,
        None => Run::boot(Env::new(tag), &refs(&v), false, false).await,
    };
    let (supervisor, _) = row_with_supervisor(&run, &mut fx, "kept").await;
    // The check runs on a connection of its own and does not answer while the stub hangs.
    let check = if automatic {
        open(case.path(), "update-check-go");
        None
    } else {
        let socket = run.env.socket_path.clone();
        Some(tokio::spawn(async move {
            let (mut conn, id) = connect_and_hello(&socket).await;
            let _ = call(&mut conn, id, op::UPDATE_CHECK, serde_json::json!({})).await;
        }))
    };
    let ids = watch_stub(&mut fx, case.path(), program).await;
    fx.save("before", ids.iter().all(|i| alive(&fx, *i)));
    run.daemon_does("raise:9");
    if let Some(check) = check {
        check.abort();
    }
    let mut run = run;
    let status = run.status_within(Duration::from_secs(60)).await;
    fx.save("status", describe(status));
    fx.save("ended", all_ended(&fx, &ids, Duration::from_secs(10)));
    fx.save("capsule_alive", alive(&fx, supervisor));
    let said = run.said();
    // The capsule stays by design: the window's close, through a successor, ends it.
    run.end_capsules().await;
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_eq!(
        fx.saved("before"),
        Some("true"),
        "the {program} stub was not running when the daemon ended:\n{said}"
    );
    assert_eq!(fx.saved("status"), Some("signal 9"), "{said}");
    assert_eq!(
        fx.saved("ended"),
        Some("true"),
        "the updater's {program} tree outlived the daemon"
    );
    assert_eq!(
        fx.saved("capsule_alive"),
        Some("true"),
        "the daemon's end took the capsule with it"
    );
}

#[tokio::test]
async fn the_discovery_command_ends_with_the_daemon() {
    update_command_ends_with_the_daemon("curl", false, "updcurl").await;
}

#[tokio::test]
async fn the_prepare_command_ends_with_the_daemon() {
    update_command_ends_with_the_daemon("git", true, "updgit").await;
}

/// The automatic update, end to end on real programs: the check finds the release in the folder, the stage extracts the
/// archive with `tar`, the prepare adds the tag's worktree with `git`, the arm writes the pointer, and with no window
/// attached the daemon exits 75. The REPL's tree ends and the capsule stays.
#[tokio::test]
#[ignore = "needs a Julia 1.12 (SOT_JULIA_BIN, else julia); run by the harness job with --ignored"]
async fn the_automatic_update_exits_75_when_no_window_is_attached() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("update_auto_open_restart_75");
    let case = tempfile::tempdir().unwrap();
    let release = update_fixture::build(&case.path().join("release-fixture"));
    let mut v = vars(case.path(), "auto", true);
    v.push(("SOT_UPDATE_FETCHER".to_string(), release.fetcher.clone()));
    // The cell starts the tree and returns: no request is open on the daemon when the automatic decision comes.
    let mut spinning = start_spinning_at(
        "updauto",
        &mut fx,
        false,
        &refs(&v),
        Some(&release.daemon),
        false,
    )
    .await;
    let (pid, created) = supervisor_in(&spinning.run.env, &spinning.state_dir)
        .await
        .expect("the capsule's supervisor answers");
    let supervisor = fx
        .watch(pid, Some(created), "the capsule's supervisor")
        .expect("an identity for the reported supervisor");
    open(case.path(), "update-check-go");
    let status = spinning.run.status_within(Duration::from_secs(180)).await;
    fx.save("status", describe(status));
    fx.save(
        "tree_ended",
        all_ended(&fx, &spinning.ids, Duration::from_secs(10)),
    );
    fx.save("capsule_alive", alive(&fx, supervisor));
    let said = spinning.run.said();
    // The capsule stays by design: the window's close, through a successor, ends it.
    spinning.run.end_capsules().await;
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert!(
        said.contains("auto mode: armed and no clients attached"),
        "the update was not armed through the pipeline:\n{said}"
    );
    assert_eq!(fx.saved("status"), Some("exit 75"), "{said}");
    assert_eq!(
        fx.saved("tree_ended"),
        Some("true"),
        "the REPL's tree outlived the update restart"
    );
    assert_eq!(
        fx.saved("capsule_alive"),
        Some("true"),
        "the update restart took the capsule with it"
    );
}

/// The same pipeline with a window attached at the automatic decision: the release is armed and the daemon stays up.
#[tokio::test]
async fn the_automatic_update_waits_while_a_window_is_attached() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("update_auto_client_attached");
    let case = tempfile::tempdir().unwrap();
    let release = update_fixture::build(&case.path().join("release-fixture"));
    let mut v = vars(case.path(), "auto", true);
    v.push(("SOT_UPDATE_FETCHER".to_string(), release.fetcher.clone()));
    let mut run = Run::boot_at(Env::new("updattach"), &release.daemon, &refs(&v)).await;
    let (supervisor, _) = row_with_supervisor(&run, &mut fx, "kept").await;
    let (_attached, _) = connect_and_hello(&run.env.socket_path).await;
    open(case.path(), "update-check-go");
    fx.save(
        "deferred",
        log_says(&run, "armed but clients attached", Duration::from_secs(90)).await,
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    fx.save(
        "still_running",
        run.status_within(Duration::ZERO).await.is_none(),
    );
    fx.save("capsule_alive", alive(&fx, supervisor));
    let armed = sot_updater::pending::read(
        &release.updates,
        sot_updater::platform::this_platform().expect("the test host is in the release matrix"),
    )
    .await;
    fx.save("armed", matches!(armed, Ok(Some(_))));
    drop(_attached);
    // The close ends the row; a daemon that exited is replaced by a successor that closes: the assertions say why.
    let closed = run.end_capsules().await;
    let said = run.said();
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_eq!(fx.saved("deferred"), Some("true"), "{said}");
    assert_eq!(
        fx.saved("armed"),
        Some("true"),
        "the release was not armed:\n{said}"
    );
    assert_eq!(
        fx.saved("still_running"),
        Some("true"),
        "the daemon exited with a window attached:\n{said}"
    );
    assert_eq!(fx.saved("capsule_alive"), Some("true"));
    assert_eq!(describe(closed), "exit 0", "{said}");
}
