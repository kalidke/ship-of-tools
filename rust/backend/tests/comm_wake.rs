#![cfg(target_os = "linux")]
//! The comm wake (0031 B3), end to end: a real `sotd` runs the real tick
//! against a real capsule row whose agent is a stub `claude` script on a
//! scratch PATH. The stub draws `❯ `, logs every line it reads, and logs
//! `<ms> ping` for the wake line. The daemon's comm home is the harness's own
//! scratch `SOT_COMM_HOME`, never the live one.
//!
//! Run `cargo build -p sot-log --bin sot-capsule` into the same target dir
//! first, as for `capsule_workspaces`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sot_protocol::op;

mod support;
use support::*;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const HANDLE: &str = "wakeh";

/// How long after one appended line the wake line must have been typed.
const WAKE_WITHIN: Duration = Duration::from_secs(5);

/// The three wakes must land within one hold of each other (the spread bound for
/// `rows_with_mail_are_held_at_once`; not `comm_wake::STILL_FOR`).
const ONE_HOLD: Duration = Duration::from_secs(1);

/// A stub `claude`: banner, then `❯ ` and one line read at a time. While
/// `dialog` exists it shows a dialog instead of the prompt (checked before
/// each prompt, so the file must exist before the row starts). While `spin`
/// exists a background loop redraws the line above the prompt every 0.2 s
/// with a counter, the cursor staying just after `❯ `.
fn write_stub_claude(dir: &Path, log: &Path, dialog: &Path, spin: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).expect("mkdir stub bin");
    let claude = dir.join("claude");
    let script = format!(
        "#!/bin/sh\n\
         [ -f '{dialog}.down' ] && exit 1\n\
         echo banner\n\
         while :; do\n\
           if [ -f '{dialog}' ]; then\n\
             echo 'Allow this action? (y/n)'\n\
             while [ -f '{dialog}' ]; do sleep 0.1; done\n\
           fi\n\
           printf '\\342\\235\\257 '\n\
           ( n=0; while :; do if [ -f '{spin}' ]; then n=$((n+1)); printf '\\0337\\033[1A\\r spinner %d\\0338' $n; fi; sleep 0.2; done ) &\n\
           spid=$!\n\
           IFS= read -r line\n\
           kill $spid\n\
           case \"$line\" in\n\
             quit) exit 0 ;;
             '[sot-comm] you have mail'*) echo \"$(date +%s%3N) ping\" >> '{log}' ;;\n\
             *) echo \"$(date +%s%3N) other\" >> '{log}' ;;\n\
           esac\n\
         done\n",
        dialog = dialog.display(),
        spin = spin.display(),
        log = log.display(),
    );
    std::fs::write(&claude, script).expect("write stub claude");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod stub claude");
    dir.to_path_buf()
}

fn pings(log: &Path) -> usize {
    std::fs::read_to_string(log).map(|s| s.lines().filter(|l| l.ends_with(" ping")).count()).unwrap_or(0)
}

/// Appends `n` complete inbox lines to `HANDLE` in ONE write, as one batch.
fn append_mail(env: &Env, n: usize) {
    append_mail_to(env, HANDLE, n);
}

fn append_mail_to(env: &Env, handle: &str, n: usize) {
    let dir = env.comm_root.join("inbox");
    std::fs::create_dir_all(&dir).expect("mkdir inbox");
    let line = format!("{{\"from\":\"other\",\"to\":\"{handle}\",\"repo\":\"r\",\"msg\":\"hi\",\"ts\":\"2026-01-01T00:00:00Z\"}}\n");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{handle}.jsonl"))).expect("open inbox");
    std::io::Write::write_all(&mut f, line.repeat(n).as_bytes()).expect("append inbox");
}

async fn wait_pings(log: &Path, want: usize, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if pings(log) >= want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    pings(log) >= want
}

struct Row {
    env: Env,
    conn: Conn,
    next_id: u64,
    ws: String,
    log: PathBuf,
    dialog: PathBuf,
    spin: PathBuf,
    stub_dir: PathBuf,
}

/// A daemon with one `ready` capsule row running the stub, declaring `HANDLE`.
impl Row {
    /// Whether `needle` is on the row's screen now. The stub's tty echoes
    /// whatever is typed, so this sees a wake typed into a dialog even
    /// though the stub itself is not reading yet.
    async fn screen_has(&mut self, needle: &str) -> bool {
        let id = self.next_id;
        self.next_id += 1;
        let res = call(&mut self.conn, id, op::PTY_SCREEN, serde_json::json!({ "workspace_id": self.ws })).await;
        res.payload["lines"].as_array().expect("lines").iter().any(|l| l.as_str().unwrap_or_default().contains(needle))
    }
}

/// A second ready capsule row declaring `handle`.
async fn add_row(row: &mut Row, label: &str, handle: &str, root: &str) -> String {
    let root = row.env._tmp.path().join(root);
    std::fs::create_dir_all(&root).expect("mkdir second root");
    let create = serde_json::json!({
        "label": label,
        "project_root": root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
    });
    let res = call(&mut row.conn, row.next_id, op::WORKSPACE_CREATE, create).await;
    row.next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let ws = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let join = call(&mut row.conn, row.next_id, op::AGENT_JOIN, serde_json::json!({ "workspace_id": ws, "handle": handle })).await;
    row.next_id += 1;
    assert_eq!(join.payload["ok"], true, "agent.join failed: {:?}", join.payload);
    poll_for_phase(&mut row.conn, &mut row.next_id, &ws, "ready", BOUND.max(Duration::from_secs(60))).await;
    ws
}

async fn phase_of_row(row: &mut Row, ws: &str) -> String {
    let id = row.next_id;
    row.next_id += 1;
    let payload = call(&mut row.conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    find_row(&payload, ws).and_then(|r| r["phase"].as_str().map(str::to_string)).unwrap_or_default()
}

/// Three ticks (2 s each) plus slack.
const THREE_TICKS: Duration = Duration::from_secs(8);

async fn start(tag: &str, log: Option<PathBuf>, in_dialog: bool) -> Row {
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not built next to sotd: see the header");
    let env = Env::new(tag);
    let log = log.unwrap_or_else(|| env._tmp.path().join("ping.log"));
    let dialog = env._tmp.path().join("dialog");
    let spin = env._tmp.path().join("spin");
    let stub_dir = write_stub_claude(&env._tmp.path().join("stubbin"), &log, &dialog, &spin);
    if in_dialog {
        std::fs::write(&dialog, b"").unwrap();
    }
    env.spawn_sotd_with_prepended_path(&stub_dir);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let create = serde_json::json!({
        "label": "wake-row",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let ws = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let join = call(&mut conn, next_id, op::AGENT_JOIN, serde_json::json!({ "workspace_id": ws, "handle": HANDLE })).await;
    next_id += 1;
    assert_eq!(join.payload["ok"], true, "agent.join failed: {:?}", join.payload);
    poll_for_phase(&mut conn, &mut next_id, &ws, "ready", BOUND.max(Duration::from_secs(60))).await;
    Row { env, conn, next_id, ws, log, dialog, spin, stub_dir }
}

/// An idle row is woken within 5 s of one appended line, and only once.
#[tokio::test]
async fn an_idle_row_is_woken_once() {
    let _serial = SERIAL.lock().await;
    let row = start("cwi", None, false).await;
    append_mail(&row.env, 1);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?}");
    // The mail is still unread; the same batch is not woken again.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(pings(&row.log), 1);
    row.env.kill_daemon_bounded().await;
}

/// A row in a dialog is not typed into, and is woken within 5 s of the
/// dialog closing.
#[tokio::test]
async fn a_row_in_a_dialog_is_not_typed_into_until_it_closes() {
    let _serial = SERIAL.lock().await;
    let mut row = start("cwd", None, true).await;
    append_mail(&row.env, 1);
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert!(row.screen_has("Allow this action").await, "the dialog is not on screen");
    assert!(!row.screen_has("[sot-comm]").await, "typed into a dialog");
    std::fs::remove_file(&row.dialog).unwrap();
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?} of the dialog closing");
    row.env.kill_daemon_bounded().await;
}

/// A working row (its spinner redrawing above a live prompt) is not typed
/// into, and is woken within 5 s of coming to rest.
#[tokio::test]
async fn a_working_row_is_not_typed_into_until_it_rests() {
    let _serial = SERIAL.lock().await;
    let row = start("cww", None, false).await;
    std::fs::write(&row.spin, b"").unwrap();
    append_mail(&row.env, 1);
    tokio::time::sleep(THREE_TICKS).await;
    assert_eq!(pings(&row.log), 0, "typed into a working row");
    std::fs::remove_file(&row.spin).unwrap();
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?} of the row coming to rest");
    row.env.kill_daemon_bounded().await;
}

/// Rows with mail are checked together: three rows are all woken within one
/// hold of each other, not one hold apiece.
#[tokio::test]
async fn rows_with_mail_are_held_at_once() {
    let _serial = SERIAL.lock().await;
    let mut row = start("cwh", None, false).await;
    add_row(&mut row, "wake-row-2", "wakeh2", "second").await;
    add_row(&mut row, "wake-row-3", "wakeh3", "third").await;
    for h in [HANDLE, "wakeh2", "wakeh3"] {
        append_mail_to(&row.env, h, 1);
    }
    assert!(wait_pings(&row.log, 3, WAKE_WITHIN).await, "not all three rows were woken within {WAKE_WITHIN:?}");
    let stamps: Vec<u64> = std::fs::read_to_string(&row.log)
        .unwrap()
        .lines()
        .filter(|l| l.ends_with(" ping"))
        .filter_map(|l| l.split(' ').next()?.parse().ok())
        .collect();
    let spread = stamps.iter().max().unwrap() - stamps.iter().min().unwrap();
    assert!((spread as u128) < ONE_HOLD.as_millis(), "the three wakes were {spread} ms apart");
    row.env.kill_daemon_bounded().await;
}

/// A restarted daemon with unread mail wakes the row once.
#[tokio::test]
async fn a_restarted_daemon_wakes_a_row_with_unread_mail_once() {
    let _serial = SERIAL.lock().await;
    let row = start("cwr", None, false).await;
    append_mail(&row.env, 1);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await);
    row.env.kill_daemon_bounded().await;
    row.env.spawn_sotd_with_prepended_path(&row.stub_dir);
    assert!(wait_pings(&row.log, 2, Duration::from_secs(30)).await, "the restarted daemon never woke the row");
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(pings(&row.log), 2);
    row.env.kill_daemon_bounded().await;
}

/// Three lines appended within one tick give exactly one wake line.
#[tokio::test]
async fn one_batch_gives_one_line() {
    let _serial = SERIAL.lock().await;
    let row = start("cwb", None, false).await;
    append_mail(&row.env, 3);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(pings(&row.log), 1);
    row.env.kill_daemon_bounded().await;
}

/// Two rows declaring one handle: a wake aimed by a guess would type into
/// someone else's session, so neither is woken (`comm_wake::run`).
#[tokio::test]
async fn two_rows_on_one_handle_are_not_woken() {
    let _serial = SERIAL.lock().await;
    let mut row = start("cw2", None, false).await;
    add_row(&mut row, "wake-row-2", HANDLE, "second").await;
    append_mail(&row.env, 1);
    tokio::time::sleep(THREE_TICKS).await;
    assert_eq!(pings(&row.log), 0, "a row on a shared handle was woken");
    row.env.kill_daemon_bounded().await;
}

/// A row whose agent has exited holds unread mail: the wake types nothing and
/// never restarts it. Nothing on the wake path restarts a row; this guards the
/// outcome.
#[tokio::test]
async fn a_dead_row_is_never_restarted_by_a_wake() {
    let _serial = SERIAL.lock().await;
    let mut row = start("cwx", None, false).await;
    let ws = row.ws.clone();
    std::fs::write(row.dialog.with_extension("down"), b"").unwrap();
    let quit = serde_json::json!({ "workspace_id": ws, "data_b64": "cXVpdA==", "enter": true });
    let res = call(&mut row.conn, row.next_id, op::PTY_INPUT, quit).await;
    row.next_id += 1;
    assert_eq!(res.payload["ok"], true, "pty.input failed: {:?}", res.payload);
    let deadline = Instant::now() + Duration::from_secs(30);
    // The supervisor respawns a stub that exits, so the stub is made to exit
    // at every start until the row rests in a phase other than ready.
    let deadline = Instant::now() + Duration::from_secs(60);
    let before = loop {
        let phase = phase_of_row(&mut row, &ws).await;
        if phase == "terminal" || phase == "ended_no_respawn" {
            break phase;
        }
        assert!(Instant::now() < deadline, "the row never came to rest dead (phase {phase})");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    // Unblock the stub, so a restart WOULD let it draw a prompt and be woken.
    std::fs::remove_file(row.dialog.with_extension("down")).unwrap();
    append_mail(&row.env, 1);
    tokio::time::sleep(THREE_TICKS).await;
    assert_eq!(pings(&row.log), 0, "a dead row was typed a wake");
    assert_eq!(phase_of_row(&mut row, &ws).await, before, "the wake restarted the row");
    row.env.kill_daemon_bounded().await;
}

/// The e2e harness's 5a (`test-comm-e2e-readers.sh`): the real tick against a
/// free stub row. `SOT_E2E_PING_LOG` names the file the stub appends
/// `<ms> ping` to for the wake line; the mail is filed here, after the row is
/// ready, and the harness measures from the log.
#[tokio::test]
#[ignore = "run by test-comm-e2e-readers.sh with SOT_E2E_PING_LOG set"]
async fn comm_wake_e2e() {
    let _serial = SERIAL.lock().await;
    let log = PathBuf::from(std::env::var_os("SOT_E2E_PING_LOG").expect("SOT_E2E_PING_LOG names the ping log"));
    let row = start("cwe", Some(log.clone()), false).await;
    append_mail(&row.env, 1);
    assert!(wait_pings(&log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?}");
    row.env.kill_daemon_bounded().await;
}
