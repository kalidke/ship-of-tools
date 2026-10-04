#![cfg(any(windows, target_os = "linux"))]
//! A daemon's start from `held.json` (`startup::begin`), end to end
//! against real daemons: an absent record resumes as before; a record
//! that is unreadable, says `closing`, or comes from another boot ends
//! every row without resuming any; recorded holders and a handover resume
//! at once and keep their deadline across the restart. Every daemon here
//! is a scratch daemon on its own `Env`; "killed" is a SIGKILL of that
//! daemon alone, so its rows' supervisors live on into the next start.

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sot_protocol::ops::{FeLeaseReq, FeLeaseRes, LeaseOutcome};
use sot_protocol::op;
use support::{
    call, connect_and_hello, find_row, poll_for_phase, poll_until, try_connect, try_query_status, Conn, Env, BOUND,
    TEST_STATE_HOST,
};

/// `Env::new` sets this process's `SOT_RUNTIME_DIR`, which the test's own
/// `query_status` reads: one test at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The handover bound the holders tests' daemons run with
/// (`SOT_TEST_HANDOVER_BOUND_MS`), short enough to wait out.
const HANDOVER_MS: u64 = 5000;

/// The countdown test's bound: long enough that a restart 3 s into it is
/// still pending, and that a deadline the restart moved lands well after
/// the recorded one.
const COUNTDOWN_MS: u64 = 8000;

/// `sot_state_dir()` under `env`'s `XDG_STATE_HOME`/`LOCALAPPDATA`.
fn state_dir(env: &Env) -> PathBuf {
    env.state_root.join("sot")
}

fn record_path(env: &Env) -> PathBuf {
    state_dir(env).join(sot_protocol::ops::lease::HELD_RECORD_FILE)
}

fn read_record(env: &Env) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(record_path(env)).ok()?;
    Some(serde_json::from_str(&text).expect("held.json parses"))
}

/// A record as a shutdown's step 1 leaves it: `closing`, this boot.
fn write_closing_record(env: &Env) {
    write_record(env, true, &[], 0);
}

/// A record of this boot with no holder and no deadline.
fn write_record(env: &Env, closing: bool, forget: &[&str], not_ended: u32) {
    let rec = serde_json::json!({
        "v": 1,
        "boot": sot_log::identity::challenge::boot_identity().unwrap_or_default(),
        "holders": [],
        "handover_until_ms": null,
        "closing": closing,
        "not_ended": not_ended,
        "forget": forget,
    });
    std::fs::create_dir_all(state_dir(env)).unwrap();
    std::fs::write(record_path(env), serde_json::to_vec(&rec).unwrap()).unwrap();
}

fn row_toml(env: &Env, slug: &str) -> PathBuf {
    env.app_config_dir().join(format!("workspaces-{TEST_STATE_HOST}")).join(format!("{slug}.toml"))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// A capsule row that reached `ready` on `env`'s daemon.
struct Row {
    id: String,
    slug: String,
    state_dir: PathBuf,
    /// The answering supervisor's pid.
    pid: u32,
}

async fn ready_row(env: &Env, label: &str, agent: Option<&str>) -> Row {
    ready_row_in(env, label, agent, &env.workspace_project_root).await
}

/// A project root of its own under the scratch workspace root.
#[cfg(target_os = "linux")]
fn own_root(env: &Env, label: &str) -> PathBuf {
    let root = env.workspace_project_root.join(label);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// `ready_row` under its own project root: two rows cannot share one.
async fn ready_row_in(env: &Env, label: &str, agent: Option<&str>, root: &Path) -> Row {
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let mut req = serde_json::json!({
        "label": label,
        "project_root": root.to_string_lossy(),
        "runtime": "capsule",
    });
    // `none` is a bash row: it runs until it is ended.
    req["agent"] = agent.unwrap_or("none").into();
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, req).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let id = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    poll_for_phase(&mut conn, &mut next_id, &id, "ready", BOUND).await;
    let list = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await;
    let row = find_row(&list.payload, &id).expect("workspace.list row");
    let state_dir = PathBuf::from(row["state_dir"].as_str().expect("state_dir"));
    let slug = row["slug"].as_str().expect("slug").to_string();
    let pid = poll_until(|| supervisor_pid(&state_dir), BOUND, "a ready row's supervisor to answer").await;
    Row { id, slug, state_dir, pid }
}

async fn supervisor_pid(state_dir: &Path) -> Option<u32> {
    let dir = state_dir.to_path_buf();
    tokio::task::spawn_blocking(move || sot_log::attach_client::supervisor_client::query_status(&dir).ok().map(|(_, p)| p.pid()))
        .await
        .unwrap()
}

async fn workspace_list(env: &Env) -> serde_json::Value {
    let (mut conn, next_id) = connect_and_hello(&env.socket_path).await;
    call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload
}

/// The row was ended without a resume and forgotten, and the daemon is
/// still up to say so.
async fn assert_ended_and_forgotten(env: &Env, row: &Row, why: &str) {
    poll_until(
        || async { try_query_status(row.state_dir.clone()).await.is_none().then_some(()) },
        BOUND,
        &format!("{why}: the start to end row {} (its supervisor still answers; the start resumed it)", row.id),
    )
    .await;
    poll_until(
        || async { (!row_toml(env, &row.slug).exists()).then_some(()) },
        BOUND,
        &format!("{why}: the start to forget row {} (its toml remains)", row.id),
    )
    .await;
    poll_until(
        || async { find_row(&workspace_list(env).await, &row.id).is_none().then_some(()) },
        BOUND,
        &format!("{why}: the start to unregister row {}", row.id),
    )
    .await;
}

/// The row still runs under the supervisor it had before the restart.
async fn assert_adopted(env: &Env, row: &Row, why: &str) {
    let pid = poll_until(
        || async { supervisor_pid(&row.state_dir).await },
        BOUND,
        &format!("{why}: row {}'s supervisor to answer", row.id),
    )
    .await;
    assert_eq!(pid, row.pid, "{why}: row {} was not adopted: its supervisor changed", row.id);
    assert!(row_toml(env, &row.slug).exists(), "{why}: row {}'s toml is gone", row.id);
}

/// Kill -9 every leg of `row`, its supervisor and its run, by the
/// pattern anchored on the row's own state dir: the row's capsule dies
/// with no end, as a crash leaves it.
#[cfg(target_os = "linux")]
fn kill_row_capsule(row: &Row) {
    let legs = row_legs(row);
    assert!(support::any_process_matches(&legs), "no leg of row {} matches {legs}", row.id);
    let _ = std::process::Command::new("pkill").args(["-9", "-f", &legs]).status();
    assert!(support::poll_until_no_process_matches(&legs, BOUND), "row {}'s legs survived the kill", row.id);
}

#[cfg(target_os = "linux")]
fn row_legs(row: &Row) -> String {
    support::build_leg_pgrep_pattern(&support::sot_capsule_exe(), "(supervise|run)", &row.state_dir)
}

/// The "slow row": its legs killed, then its fence held here, so no end
/// of it can be proven until the fence is dropped.
#[cfg(target_os = "linux")]
async fn hold_row_end(row: &Row) -> sot_log::supervisor::journal::fence::SupervisorLock {
    kill_row_capsule(row);
    let dir = row.state_dir.clone();
    poll_until(|| { let dir = dir.clone(); async move { sot_log::supervisor::journal::fence::lock_supervisor(&dir).ok() } }, BOUND, "the row's fence").await
}

/// No sot-capsule leg of `row` appears for 3 s: the start never resumed it.
#[cfg(target_os = "linux")]
fn assert_never_resumed(row: &Row, why: &str) {
    let legs = row_legs(row);
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        assert!(
            !support::any_process_matches(&legs),
            "{why}: row {} was resumed: a sot-capsule appeared for its state dir",
            row.id
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The daemon's exit status, waited for up to `within`.
fn wait_exit(env: &Env, within: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + within;
    loop {
        let status = env.daemon.borrow_mut().as_mut().expect("a spawned daemon").try_wait().unwrap();
        if status.is_some() || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A lease held by this test process, as a window holds one.
async fn lease(env: &Env) -> (Conn, FeLeaseRes) {
    let stream = poll_until(|| try_connect(&env.socket_path), BOUND, "the daemon's socket").await;
    let mut conn = tokio::io::BufReader::new(stream);
    let me = sot_log::identity::challenge::self_identity().expect("self identity");
    let req = FeLeaseReq { boot: me.boot, pid: me.pid, created: me.created, token: None };
    let res = call(&mut conn, 1, op::FE_LEASE, serde_json::to_value(&req).unwrap()).await;
    let res: FeLeaseRes = serde_json::from_value(res.payload.clone())
        .unwrap_or_else(|e| panic!("fe.lease reply is not a lease reply ({e}): {:?}", res.payload));
    (conn, res)
}

async fn granted_lease(env: &Env) -> Conn {
    let (conn, res) = lease(env).await;
    assert_eq!(res.outcome, LeaseOutcome::Granted, "lease refused: {res:?}");
    conn
}

async fn leaving(conn: &mut Conn, id: u64, intent: &str) {
    let res = call(conn, id, op::FE_LEAVING, serde_json::json!({ "intent": intent })).await;
    assert!(res.payload.get("error").is_none(), "fe.leaving {intent} failed: {:?}", res.payload);
}

#[tokio::test]
async fn unheld_resumes_as_today() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("unheld");
    env.spawn_sotd();
    let row = ready_row(&env, "unheld-row", None).await;
    env.kill_daemon_bounded().await;
    assert!(read_record(&env).is_none(), "no lease was held, yet a record exists");

    env.spawn_sotd();
    assert_adopted(&env, &row, "an absent record").await;
    env.kill_daemon_bounded().await;
}

#[tokio::test]
async fn keep_deletes_record() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("keep");
    env.spawn_sotd();
    let row = ready_row(&env, "keep-row", None).await;
    let mut conn = granted_lease(&env).await;
    assert!(read_record(&env).is_some_and(|r| r["holders"].as_array().is_some_and(|h| h.len() == 1)), "the grant was not recorded");

    leaving(&mut conn, 2, "keep").await;
    poll_until(|| async { read_record(&env).is_none().then_some(()) }, BOUND, "a keep to delete the record").await;
    drop(conn);
    env.kill_daemon_bounded().await;

    env.spawn_sotd();
    assert_adopted(&env, &row, "the start after a keep").await;
    env.kill_daemon_bounded().await;
}

#[tokio::test]
async fn unreadable_record_cleans_up() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("unreadable");
    env.spawn_sotd();
    let row = ready_row(&env, "unreadable-row", None).await;
    env.kill_daemon_bounded().await;
    std::fs::write(record_path(&env), b"{ not a record").unwrap();

    env.spawn_sotd();
    assert_ended_and_forgotten(&env, &row, "an unreadable record").await;
    poll_until(|| async { read_record(&env).is_none().then_some(()) }, BOUND, "the Cleanup to delete the unreadable record").await;
    assert!(wait_exit(&env, Duration::ZERO).is_none(), "the daemon exited after its Cleanup");
    env.kill_daemon_bounded().await;
}

/// #11: a start that plans Cleanup never starts a row, not even to end it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn startup_cleanup_never_resumes() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("never");
    env.spawn_sotd();
    let row = ready_row(&env, "never-row", None).await;
    env.kill_daemon_bounded().await;
    kill_row_capsule(&row);
    write_closing_record(&env);

    env.spawn_sotd();
    let _ = workspace_list(&env).await;
    assert_never_resumed(&row, "a start that planned Cleanup");
    assert_ended_and_forgotten(&env, &row, "a closing record over a dead capsule").await;
    env.kill_daemon_bounded().await;
}

/// E4 (e): a row the record says to forget is dropped at every start,
/// before any resume, whatever the plan.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn forgotten_rows_never_resume() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("forget");
    env.spawn_sotd();
    let row = ready_row(&env, "forget-row", None).await;
    env.kill_daemon_bounded().await;
    kill_row_capsule(&row);
    write_record(&env, false, &[&row.id], 0);

    env.spawn_sotd();
    let listed = find_row(&workspace_list(&env).await, &row.id).is_some();
    assert_never_resumed(&row, "a start with forgotten rows");
    assert!(!listed, "the start still registers forgotten row {}", row.id);
    assert!(!row_toml(&env, &row.slug).exists(), "forgotten row {}'s toml remains", row.id);
    env.kill_daemon_bounded().await;
}

/// #16: a daemon killed between shutdown steps 1 and 5 leaves `closing`;
/// the next start finishes the shutdown's job and stays up.
#[tokio::test]
async fn killed_mid_shutdown_next_start_finishes() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("midshut");
    env.spawn_sotd();
    let row = ready_row(&env, "midshut-row", None).await;
    env.kill_daemon_bounded().await;
    write_closing_record(&env);

    env.spawn_sotd();
    assert_ended_and_forgotten(&env, &row, "a closing record").await;
    poll_until(|| async { read_record(&env).is_none().then_some(()) }, BOUND, "the Cleanup to clear `closing`").await;
    assert!(wait_exit(&env, Duration::ZERO).is_none(), "the daemon exited after its Cleanup");
    env.kill_daemon_bounded().await;
}

/// A daemon killed after it captured a row's scope leaves the row's
/// `row-scopes`; the next start's Cleanup ends that scope, escapee and
/// all, gracefully while the supervisor answers and by `cgroup.kill` when
/// the supervisor is gone (`absence_proof` says Unheld), and removes the
/// file only once the scope is empty.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn killed_after_capture_next_start_ends_scope() {
    let _serial = SERIAL.lock().await;
    use support::{arm_scope_guard, assert_scope_empties, cgroup_rel, user_manager_available_for_test};
    if let Err(e) = user_manager_available_for_test() {
        if std::env::var("SOT_TEST_REQUIRE_USER_MANAGER").as_deref() == Ok("1") {
            panic!("SOT_TEST_REQUIRE_USER_MANAGER=1 but no user manager is reachable: {e}");
        }
        eprintln!("SKIPPED: no user manager: {e}");
        return;
    }
    for supervisor_answers in [true, false] {
        let env = Env::new(if supervisor_answers { "capgrace" } else { "capunheld" });
        let (dir, pidfile) = env.seed_fake_claude_with_escapee();
        env.spawn_sotd_with_prepended_path(&dir);
        let row = ready_row(&env, "capture-row", Some("claude")).await;
        // The supervisor runs in the row's scope, as the agent does.
        let scope = cgroup_rel(row.pid);
        let _guard = arm_scope_guard(&scope, &row.state_dir);
        let escapee: u32 = poll_until(
            || async { std::fs::read_to_string(&pidfile).ok().and_then(|t| t.trim().parse().ok()) },
            BOUND,
            "the escapee's pid file",
        )
        .await;
        assert_eq!(cgroup_rel(escapee), scope, "the escapee is not in the row's scope");
        // What a daemon killed after an end's capture leaves: the row's
        // scope listed, the row registered.
        let scopes_file = row.state_dir.join("row-scopes");
        std::fs::write(&scopes_file, format!("{scope}\n")).unwrap();

        env.kill_daemon_bounded().await;
        if !supervisor_answers {
            kill_row_capsule(&row);
            assert!(
                Path::new(&format!("/proc/{escapee}")).exists(),
                "the escapee died with the capsule; the scope's end would prove nothing"
            );
        }
        write_closing_record(&env);
        env.spawn_sotd_with_prepended_path(&dir);

        assert_scope_empties(&scope, BOUND).await;
        poll_until(
            || async { (!scopes_file.exists()).then_some(()) },
            BOUND,
            "the emptied scope's row-scopes file to be removed",
        )
        .await;
        poll_until(
            || async { find_row(&workspace_list(&env).await, &row.id).is_none().then_some(()) },
            BOUND,
            "the startup Cleanup to forget the captured row",
        )
        .await;
        env.kill_daemon_bounded().await;
    }
}

/// Lite: recorded holders resume at once, the same supervisor, and
/// nothing is ended while the holder re-leases inside the bound.
#[tokio::test]
async fn restart_with_holders_resumes_at_once() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("holdres");
    let bound = HANDOVER_MS.to_string();
    env.spawn_sotd_with_env(&[("SOT_TEST_HANDOVER_BOUND_MS", &bound)]);
    let row = ready_row(&env, "holdres-row", None).await;
    let conn = granted_lease(&env).await;
    env.kill_daemon_bounded().await;
    drop(conn);

    env.spawn_sotd_with_env(&[("SOT_TEST_HANDOVER_BOUND_MS", &bound)]);
    assert_adopted(&env, &row, "recorded holders, before any re-lease").await;
    // The supervisor answers before the new daemon starts: wait for the
    // start itself to write the pending deadline.
    let hold = poll_until(
        || async { read_record(&env).and_then(|r| r["handover_until_ms"].as_u64()) },
        BOUND,
        "the pending start to record its deadline",
    )
    .await;
    let mut conn = granted_lease(&env).await;
    while now_ms() < hold + 2000 {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(wait_exit(&env, Duration::ZERO).is_none(), "the daemon shut down although its holder re-leased");
    assert_adopted(&env, &row, "past the hold, after the re-lease").await;
    leaving(&mut conn, 2, "keep").await;
    env.kill_daemon_bounded().await;
}

/// Lite: recorded holders that never re-lease are a shutdown at the
/// hold: exit 0, and the rows are ended. The recorded holder is this test
/// process, alive with its true identity throughout, so this is also #6: a
/// live process that does not re-lease never holds the rows.
#[tokio::test]
async fn restart_with_holders_no_lease_shuts_down() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("holdgone");
    let bound = HANDOVER_MS.to_string();
    env.spawn_sotd_with_env(&[("SOT_TEST_HANDOVER_BOUND_MS", &bound)]);
    let row = ready_row(&env, "holdgone-row", None).await;
    let conn = granted_lease(&env).await;
    env.kill_daemon_bounded().await;
    drop(conn);
    let me = sot_log::identity::challenge::self_identity().expect("self identity");
    let rec = read_record(&env).expect("the holder is recorded");
    assert_eq!(rec["holders"][0]["pid"], me.pid, "the recorded holder is not this live process: {rec}");
    assert_eq!(rec["holders"][0]["created"], me.created, "the recorded holder is not this live process: {rec}");

    env.spawn_sotd_with_env(&[("SOT_TEST_HANDOVER_BOUND_MS", &bound)]);
    assert_adopted(&env, &row, "recorded holders, before the hold").await;
    let status = wait_exit(&env, Duration::from_millis(HANDOVER_MS) + BOUND).expect("no shutdown at the hold");
    assert_eq!(status.code(), Some(0), "the hold's shutdown exit: {status:?}");
    assert!(try_query_status(row.state_dir.clone()).await.is_none(), "the shutdown left the row's supervisor running");
    assert!(!row_toml(&env, &row.slug).exists(), "the shutdown left the row registered");
}

/// #1: a handover's deadline is the one recorded before the restart; a
/// lease inside it keeps everything running.
#[tokio::test]
async fn handover_countdown_survives_restart() {
    let _serial = SERIAL.lock().await;
    for lease_in_time in [false, true] {
        let env = Env::new(if lease_in_time { "handin" } else { "handout" });
        let bound = COUNTDOWN_MS.to_string();
        env.spawn_sotd_with_env(&[("SOT_TEST_HANDOVER_BOUND_MS", &bound)]);
        let row = ready_row(&env, "hand-row", None).await;
        let mut conn = granted_lease(&env).await;
        leaving(&mut conn, 2, "handover").await;
        let until = read_record(&env)
            .and_then(|r| r["handover_until_ms"].as_u64())
            .expect("the handover's deadline is recorded");
        env.kill_daemon_bounded().await;
        drop(conn);
        // Restarted late enough that a deadline the restart moved would
        // fall clearly after the recorded one.
        tokio::time::sleep(Duration::from_secs(3)).await;

        env.spawn_sotd_with_env(&[("SOT_TEST_HANDOVER_BOUND_MS", &bound)]);
        assert_adopted(&env, &row, "a pending handover").await;
        if lease_in_time {
            let mut conn = granted_lease(&env).await;
            while now_ms() < until + 3000 {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            assert!(wait_exit(&env, Duration::ZERO).is_none(), "the daemon shut down although a lease arrived in time");
            assert_adopted(&env, &row, "a handover a lease completed").await;
            leaving(&mut conn, 2, "keep").await;
            env.kill_daemon_bounded().await;
        } else {
            let status = wait_exit(&env, Duration::from_millis(COUNTDOWN_MS) + BOUND).expect("no shutdown at the handover");
            let at = now_ms();
            assert_eq!(status.code(), Some(0), "the handover's shutdown exit: {status:?}");
            assert!(at >= until, "the shutdown came before the recorded deadline");
            assert!(at < until + 3000, "the restart moved the handover's deadline: shut down {} ms after it", at - until);
            assert!(!row_toml(&env, &row.slug).exists(), "the shutdown left the row registered");
        }
    }
}

/// #26: a row whose registration will not go is never resumed: it is
/// recorded in `forget`, and the next start drops it before any resume.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn unremovable_registration_is_never_resumed() {
    use std::os::unix::fs::PermissionsExt;

    /// Puts the directory back to 0755 on drop, so the tempdir can go.
    struct Mode(PathBuf);
    impl Drop for Mode {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    let _serial = SERIAL.lock().await;
    let env = Env::new("unremovable");
    env.spawn_sotd_with_env(&[("SOT_TEST_SHUTDOWN_BOUND_MS", "15000")]);
    let row = ready_row(&env, "unremovable-row", None).await;
    let conn = granted_lease(&env).await;

    let toml = row_toml(&env, &row.slug);
    let dir = toml.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let guard = Mode(dir.clone());
    assert!(
        std::fs::File::create(dir.join("probe")).is_err(),
        "directory permissions are not enforced for this user (root?); this test would prove nothing"
    );

    // The last lease's EOF is a Close.
    drop(conn);
    let status = wait_exit(&env, BOUND).expect("no shutdown after the last lease left");
    assert_eq!(status.code(), Some(0), "the shutdown's exit: {status:?}");
    let rec = read_record(&env).expect("the shutdown left a record");
    assert!(
        rec["forget"].as_array().is_some_and(|f| f.iter().any(|id| id == row.id.as_str())),
        "the row whose registration would not go is not in the record's forget: {rec}"
    );
    assert!(toml.exists(), "the toml went although its directory is read-only");
    assert!(try_query_status(row.state_dir.clone()).await.is_none(), "the shutdown left the row's supervisor running");

    drop(guard);
    env.kill_daemon_bounded().await;
    env.spawn_sotd();
    assert_never_resumed(&row, "a record that forgets the row");
    assert!(find_row(&workspace_list(&env).await, &row.id).is_none(), "the start still registers the forgotten row");
    assert!(!toml.exists(), "the forgotten row's toml remains");

    let mut conn = granted_lease(&env).await;
    let rec = read_record(&env).expect("the grant is recorded");
    assert!(rec["forget"].as_array().is_none_or(|f| f.is_empty()), "the forgotten row is still in the record: {rec}");
    leaving(&mut conn, 2, "keep").await;
    env.kill_daemon_bounded().await;
}

/// ADR 0050 Start step 5: a daemon killed during a startup Cleanup leaves
/// the record `closing`, so the next start runs the Cleanup again, which
/// fails toward close: a row created or leased-over mid-Cleanup is ended
/// too, and none is resumed.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn cleanup_interrupted_by_kill_never_resumes() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("cleankill");
    env.spawn_sotd();
    let a = ready_row(&env, "old-a", None).await;
    let b = ready_row_in(&env, "old-b", None, &own_root(&env, "old-b")).await;
    env.kill_daemon_bounded().await;
    let fence = hold_row_end(&a).await;
    write_closing_record(&env);

    // B is ended promptly; A is held by the fence.
    env.spawn_sotd_with_env(&[("SOT_TEST_SHUTDOWN_BOUND_MS", "60000")]);
    assert_ended_and_forgotten(&env, &b, "the first Cleanup").await;
    let conn = granted_lease(&env).await;
    let n = ready_row_in(&env, "new-n", None, &own_root(&env, "new-n")).await;
    assert!(
        read_record(&env).is_some_and(|r| r["closing"] == true),
        "a grant during a startup Cleanup rewrote the record"
    );

    env.kill_daemon_bounded().await;
    drop(conn);
    drop(fence);
    env.spawn_sotd();
    assert_never_resumed(&a, "the re-run Cleanup");
    assert_ended_and_forgotten(&env, &a, "the re-run Cleanup").await;
    assert_ended_and_forgotten(&env, &n, "the re-run Cleanup fails toward close (ADR 0050 Start step 5)").await;
    assert!(!row_toml(&env, &b.slug).exists(), "the first Cleanup's row came back");
    assert!(find_row(&workspace_list(&env).await, &b.id).is_none(), "the first Cleanup's row came back");

    poll_until(|| async { read_record(&env).is_none().then_some(()) }, BOUND, "the re-run Cleanup to clear `closing`").await;
    let mut conn = granted_lease(&env).await;
    leaving(&mut conn, 2, "keep").await;
    env.kill_daemon_bounded().await;
}

/// A row a startup Cleanup cannot end by its bound reaches the next
/// grant's count, and stays registered.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn not_ended_after_bound_is_counted() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("cleancount");
    env.spawn_sotd();
    let a = ready_row(&env, "count-a", None).await;
    env.kill_daemon_bounded().await;
    let fence = hold_row_end(&a).await;
    write_closing_record(&env);

    env.spawn_sotd_with_env(&[("SOT_TEST_SHUTDOWN_BOUND_MS", "5000")]);
    let n = poll_until(
        || async { read_record(&env).and_then(|r| r["not_ended"].as_u64()).filter(|n| *n >= 1) },
        BOUND,
        "the Cleanup to record a row it could not end",
    )
    .await;
    assert!(read_record(&env).is_some_and(|r| r["closing"] == false), "the finished Cleanup left `closing` set");

    let (mut conn, res) = lease(&env).await;
    assert_eq!(res.outcome, LeaseOutcome::Granted, "lease refused: {res:?}");
    assert_eq!(u64::from(res.not_ended), n, "the grant did not count the row the Cleanup could not end");
    assert!(find_row(&workspace_list(&env).await, &a.id).is_some(), "a row that was not ended is no longer registered");
    assert!(row_toml(&env, &a.slug).exists(), "a row that was not ended lost its toml");

    drop(fence);
    leaving(&mut conn, 2, "keep").await;
    env.kill_daemon_bounded().await;
}

/// #13: the count a start loads survives a grant no window acked, and a
/// restart, until `fe.notice_seen` clears it.
#[tokio::test]
async fn not_ended_survives_unacked_lease() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("unacked");
    write_record(&env, false, &[], 1);
    env.spawn_sotd();

    let (mut a, res) = lease(&env).await;
    assert_eq!(res.outcome, LeaseOutcome::Granted, "lease refused: {res:?}");
    assert_eq!(res.not_ended, 1, "the first grant did not carry the count");
    leaving(&mut a, 2, "keep").await;
    drop(a);

    env.kill_daemon_bounded().await;
    assert_eq!(
        read_record(&env).map(|r| r["not_ended"].clone()),
        Some(serde_json::json!(1)),
        "an unacked grant cleared the count"
    );
    env.spawn_sotd();
    let (mut b, res) = lease(&env).await;
    assert_eq!(res.outcome, LeaseOutcome::Granted, "lease refused: {res:?}");
    assert_eq!(res.not_ended, 1, "the count did not survive a restart");
    let r = call(&mut b, 2, op::FE_NOTICE_SEEN, serde_json::json!({ "not_ended": 1 })).await;
    assert!(r.payload.get("error").is_none(), "fe.notice_seen failed: {:?}", r.payload);

    let (mut c, res) = lease(&env).await;
    assert_eq!(res.outcome, LeaseOutcome::Granted, "lease refused: {res:?}");
    assert_eq!(res.not_ended, 0, "the acked count came back");
    assert!(
        read_record(&env).is_none_or(|r| r["not_ended"] == 0),
        "the record still carries the acked count"
    );
    leaving(&mut c, 2, "keep").await;
    leaving(&mut b, 3, "keep").await;
    env.kill_daemon_bounded().await;
}

/// #20: a launcher's lease holds the daemon up across a converge: the
/// window's handover is not a shutdown while the launcher holds a lease,
/// and the launcher re-leases after the daemon restarts.
#[tokio::test]
async fn converge_lease_survives_daemon_restart() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("converge");
    let bound = HANDOVER_MS.to_string();
    env.spawn_sotd_with_env(&[("SOT_TEST_HANDOVER_BOUND_MS", &bound)]);
    let row = ready_row(&env, "converge-row", None).await;
    let mut window = granted_lease(&env).await;
    let launcher = granted_lease(&env).await;
    leaving(&mut window, 2, "handover").await;
    drop(window);

    tokio::time::sleep(Duration::from_millis(HANDOVER_MS + 2000)).await;
    assert!(wait_exit(&env, Duration::ZERO).is_none(), "the window's handover shut down while the launcher held a lease");
    env.kill_daemon_bounded().await;
    drop(launcher);

    env.spawn_sotd_with_env(&[("SOT_TEST_HANDOVER_BOUND_MS", &bound)]);
    let started = now_ms();
    let mut launcher = granted_lease(&env).await;
    assert_adopted(&env, &row, "after the launcher re-leased").await;
    let mut window = granted_lease(&env).await;
    leaving(&mut launcher, 2, "handover").await;
    drop(launcher);
    while now_ms() < started + HANDOVER_MS + 2000 {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(wait_exit(&env, Duration::ZERO).is_none(), "the daemon shut down while the new window held a lease");
    assert_adopted(&env, &row, "past the hold, with the new window").await;
    leaving(&mut window, 2, "keep").await;
    env.kill_daemon_bounded().await;
}
