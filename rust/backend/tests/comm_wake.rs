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

/// A hang guard: these tests prove order; the e2e harness measures latency.
const WAKE_WITHIN: Duration = Duration::from_secs(30);

/// Serial checks put the first and third wakes at least two STILL_FOR holds
/// (2 x 1.5 s) apart.
const MAX_SPREAD: Duration = Duration::from_secs(3);

/// The stub's `echo-after` reader: `$1` seconds, `$2` the file the line goes to, `$3` (optional) a file the echo also
/// waits for. No echo and no line editing, so the first byte is seen when it arrives; what has arrived by then, after
/// `$1` seconds and `$3`, is printed in one go.
const READ_SLOW_ECHO: &str = r#"stty -echo -icanon min 1 time 0
IFS= read -r -N1 first
sleep "$1"
while [ -n "$3" ] && [ ! -f "$3" ]; do sleep 0.05; done
if IFS= read -r -t 0.05 -n 4096 rest; then ended=1; else ended=0; fi
printf '%s%s' "$first" "$rest"
[ $ended = 1 ] || IFS= read -r more
printf '%s%s' "$first" "$rest$more" > "$2"
stty echo icanon
printf '\r\n'
"#;

/// A stub `claude`: banner, then the real input box and one line read at a time:
/// a rule line, `❯` and a no-break space, a second rule line, the cursor back
/// on the prompt line just after `❯ `. While `dialog` exists it shows a dialog
/// instead of the prompt (checked before each prompt, so the file must exist
/// before the row starts). While `spin` exists a background loop redraws the
/// line above the top rule every 0.2 s with a counter, the cursor staying put.
/// A footer line under the lower rule, like Claude Code's background-agent
/// footer, is drawn once; while `foot` exists the loop also redraws it every
/// 0.2 s with a counter.
///
/// More control files, in `ctl` (the daemon's `SOT_TEST_WAKE_MARKS` is `ctl/marks`): `suggest` draws a dim
/// suggestion after the prompt's NBSP; `panel` draws a leader agents panel (`● main`, `◯ worker`, the
/// `← for agents` hint) under the lower rule in place of the footer, which reads free; with `panel`, `focus-hold`
/// redraws it focused (`❯ ● main`, the hint gone) once the daemon's hold has begun (`marks/hold`), and
/// `focus-after` does the same once its final check has passed (`marks/final-ok`) and then leaves the cursor on
/// the panel's first line. Rows through the box, and for `focus-hold` the cursor, stay as they were. The stub
/// writes `marks/focus-moved` at the moment it redraws for either of them. `echo-after` (its content, the seconds)
/// reads typed input without the tty's echo and prints it where the cursor is that many seconds after its first
/// byte (`READ_SLOW_ECHO`; with `focus-after` also not before the stub has moved focus), then ends the line at Enter as usual. The stub logs `ping` for exactly the wake line
/// and `other` for anything else, so a line typed twice shows.
fn write_stub_claude(dir: &Path, log: &Path, dialog: &Path, spin: &Path, foot: &Path, ctl: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).expect("mkdir stub bin");
    let claude = dir.join("claude");
    let slow = dir.join("read-slow-echo.sh");
    std::fs::write(&slow, READ_SLOW_ECHO).expect("write slow-echo reader");
    let script = format!(
        "#!/bin/sh\n\
         [ -f '{dialog}.down' ] && exit 1\n\
         rule=''; i=0; while [ $i -lt 60 ]; do rule=\"$rule$(printf '\\342\\224\\200')\"; i=$((i+1)); done\n\
         echo banner\n\
         while :; do\n\
           if [ -f '{dialog}' ]; then\n\
             echo 'Allow this action? (y/n)'\n\
             while [ -f '{dialog}' ]; do sleep 0.1; done\n\
           fi\n\
           sug=''; [ -f '{ctl}/suggest' ] && sug=$(printf '\\033[2mtry something\\033[22m')\n\
           if [ -f '{ctl}/panel' ]; then\n\
             printf '%s\\033[K\\n\\342\\235\\257\\302\\240%s\\033[K\\n%s\\033[K\\n  \\342\\227\\217 main\\033[K\\n  \\342\\227\\257 worker\\033[K\\n  \\342\\217\\265\\342\\217\\265 auto mode on \\302\\267 \\342\\206\\220 for agents\\033[K\\033[4A\\033[3G' \"$rule\" \"$sug\" \"$rule\"\n\
           else\n\
             printf '%s\\033[K\\n\\342\\235\\257\\302\\240%s\\033[K\\n%s\\033[K\\n footer 0\\033[K\\033[2A\\033[3G' \"$rule\" \"$sug\" \"$rule\"\n\
           fi\n\
           ( n=0; fh=0; fa=0; while :; do n=$((n+1)); if [ -f '{spin}' ]; then printf '\\0337\\033[2A\\r spinner %d\\033[K\\0338' $n; fi; if [ -f '{foot}' ]; then printf '\\0337\\033[2B\\r footer %d\\033[K\\0338' $n; fi; if [ -f '{ctl}/panel' ]; then if [ $fh = 0 ] && [ -f '{ctl}/marks/hold' ] && [ -f '{ctl}/focus-hold' ]; then fh=1; printf '\\0337\\033[2B\\r\\342\\235\\257 \\342\\227\\217 main\\033[K\\033[2B\\r\\033[K\\0338'; : > '{ctl}/marks/focus-moved'; fi; if [ $fa = 0 ] && [ -f '{ctl}/marks/final-ok' ] && [ -f '{ctl}/focus-after' ]; then fa=1; printf '\\0337\\033[2B\\r\\342\\235\\257 \\342\\227\\217 main\\033[K\\033[2B\\r\\033[K\\0338\\033[2B\\r'; : > '{ctl}/marks/focus-moved'; fi; fi; sleep 0.2; done ) &\n\
           spid=$!\n\
           if [ -f '{ctl}/echo-after' ]; then w=''; [ -f '{ctl}/focus-after' ] && w='{ctl}/marks/focus-moved'; bash '{slow}' \"$(cat '{ctl}/echo-after')\" '{ctl}/line' \"$w\"; line=$(cat '{ctl}/line'); else IFS= read -r line; fi\n\
           kill $spid\n\
           case \"$line\" in\n\
             quit) exit 0 ;;
             '[sot-comm] you have mail: run comm-poll.sh') echo \"$(date +%s%3N) ping\" >> '{log}' ;;\n\
             *) echo \"$(date +%s%3N) other\" >> '{log}' ;;\n\
           esac\n\
         done\n",
        dialog = dialog.display(),
        slow = slow.display(),
        spin = spin.display(),
        foot = foot.display(),
        ctl = ctl.display(),
        log = log.display(),
    );
    std::fs::write(&claude, script).expect("write stub claude");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod stub claude");
    dir.to_path_buf()
}

fn pings(log: &Path) -> usize {
    std::fs::read_to_string(log).map(|s| s.lines().filter(|l| l.ends_with(" ping")).count()).unwrap_or(0)
}

/// Lines the stub read that were not the wake line.
fn others(log: &Path) -> usize {
    std::fs::read_to_string(log).map(|s| s.lines().filter(|l| l.ends_with(" other")).count()).unwrap_or(0)
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
    foot: PathBuf,
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
    start_with(tag, log, in_dialog, &[], &[]).await
}

/// [`start`], with `ctl` control files (see [`write_stub_claude`], `name` or `name=content`) created before the row starts, and `extra`
/// env vars on the daemon. `SOT_TEST_WAKE_MARKS` is always set, to `ctl/marks`.
async fn start_with(tag: &str, log: Option<PathBuf>, in_dialog: bool, ctl: &[&str], extra: &[(&str, &str)]) -> Row {
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first (cargo build -p sot-log --bin sot-capsule)");
    let env = Env::new(tag);
    let log = log.unwrap_or_else(|| env._tmp.path().join("ping.log"));
    let dialog = env._tmp.path().join("dialog");
    let spin = env._tmp.path().join("spin");
    let foot = env._tmp.path().join("foot");
    let ctl_dir = env._tmp.path().join("ctl");
    std::fs::create_dir_all(&ctl_dir).unwrap();
    for entry in ctl {
        let (name, content) = entry.split_once('=').unwrap_or((entry, ""));
        std::fs::write(ctl_dir.join(name), content).unwrap();
    }
    let stub_dir = write_stub_claude(&env._tmp.path().join("stubbin"), &log, &dialog, &spin, &foot, &ctl_dir);
    if in_dialog {
        std::fs::write(&dialog, b"").unwrap();
    }
    let marks = ctl_dir.join("marks");
    let marks = marks.to_string_lossy().to_string();
    let mut env_vars = vec![("SOT_TEST_WAKE_MARKS", marks.as_str())];
    env_vars.extend_from_slice(extra);
    env.spawn_sotd_with_path_and_env(&stub_dir, &env_vars);
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
    Row { env, conn, next_id, ws, log, dialog, spin, foot, stub_dir }
}

/// An idle row is woken by one appended line, and only once.
#[tokio::test]
async fn an_idle_row_is_woken_once() {
    let _serial = SERIAL.lock().await;
    let row = start_with("cwi", None, false, &["suggest"], &[]).await;
    append_mail(&row.env, 1);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?}");
    // The mail is still unread; the same batch is not woken again.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(pings(&row.log), 1);
    row.env.kill_daemon_bounded().await;
}

/// A row in a dialog is not typed into, and is woken once the dialog
/// closes.
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
/// into, and is woken once it comes to rest.
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

/// An idle row whose background-agent footer ticks below the input box is
/// still woken: the hold watches only the rows through the box's lower rule.
#[tokio::test]
async fn an_idle_row_with_a_ticking_footer_is_woken() {
    let _serial = SERIAL.lock().await;
    let row = start("cwf", None, false).await;
    std::fs::write(&row.foot, b"").unwrap();
    append_mail(&row.env, 1);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?} with a ticking footer");
    row.env.kill_daemon_bounded().await;
}

/// UTC `%Y-%m-%dT%H:%M:%SZ` for now plus `off` seconds.
fn iso_at(off: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_secs() as i64;
    let out = std::process::Command::new("date")
        .args(["-u", "-d", &format!("@{}", now + off), "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .expect("run date");
    String::from_utf8(out.stdout).expect("utf8").trim().to_string()
}

/// Sets `agents.HANDLE` in the harness's registry, creating it if absent;
/// written to a temp file and renamed.
fn set_entry(env: &Env, entry: serde_json::Value) {
    let path = env.comm_root.join("registry.json");
    let mut doc: serde_json::Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({ "agents": {} }));
    doc["agents"][HANDLE] = entry;
    let tmp = env.comm_root.join("registry.json.tmp");
    std::fs::create_dir_all(&env.comm_root).expect("mkdir comm root");
    std::fs::write(&tmp, serde_json::to_vec(&doc).expect("encode")).expect("write registry tmp");
    std::fs::rename(&tmp, &path).expect("rename registry");
}

/// A row whose Stop hook is running (a fresh `stop_at`) is not typed into,
/// and is woken once the hook's `stop` has deleted the mark.
#[tokio::test]
async fn a_row_whose_stop_hook_is_running_is_not_typed_into() {
    let _serial = SERIAL.lock().await;
    let row = start("cws", None, false).await;
    set_entry(&row.env, serde_json::json!({ "state": "working", "floor": "user", "status_at": iso_at(0), "stop_at": iso_at(0) }));
    append_mail(&row.env, 1);
    tokio::time::sleep(THREE_TICKS).await;
    assert_eq!(pings(&row.log), 0, "typed into a row whose Stop hook is running");
    set_entry(&row.env, serde_json::json!({ "state": "done", "done": true, "status_at": iso_at(0) }));
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?} of the mark going");
    row.env.kill_daemon_bounded().await;
}

/// A mark past STOP_HOOK_BOUND (60 s) holds nothing: Esc, an API error or a
/// killed hook ends a turn with no `stop`.
#[tokio::test]
async fn a_stop_mark_past_the_bound_does_not_hold_the_wake() {
    let _serial = SERIAL.lock().await;
    let row = start("cwb", None, false).await;
    set_entry(&row.env, serde_json::json!({ "state": "working", "floor": "user", "status_at": iso_at(0), "stop_at": iso_at(-61) }));
    append_mail(&row.env, 1);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "an old mark held the wake");
    row.env.kill_daemon_bounded().await;
}

/// A Stop that never ends releases the row when its mark expires. The mark
/// is 40 s old, so with STOP_HOOK_BOUND = 60 it expires 20 s in: the row is
/// held through THREE_TICKS (8 s) and woken after, well inside the hang guard.
#[tokio::test]
async fn an_unfinished_stop_releases_the_row_within_the_bound() {
    let _serial = SERIAL.lock().await;
    let row = start("cwu", None, false).await;
    set_entry(&row.env, serde_json::json!({ "state": "working", "floor": "user", "status_at": iso_at(0), "stop_at": iso_at(-40) }));
    append_mail(&row.env, 1);
    tokio::time::sleep(THREE_TICKS).await;
    assert_eq!(pings(&row.log), 0, "typed into a row inside the bound");
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?} of the mark expiring");
    row.env.kill_daemon_bounded().await;
}

/// Rows with mail are checked together: three rows are all woken within two
/// holds (`MAX_SPREAD`) of each other, not one hold apiece.
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
    assert!((spread as u128) < MAX_SPREAD.as_millis(), "the three wakes were {spread} ms apart");
    row.env.kill_daemon_bounded().await;
}

/// A restarted daemon with unread mail wakes the row once.
#[tokio::test]
async fn a_restarted_daemon_wakes_a_row_with_unread_mail_once() {
    let _serial = SERIAL.lock().await;
    let row = start_with("cwr", None, false, &["suggest"], &[]).await;
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

/// Two rows joining one handle in turn: the handle moves to the newer row
/// (ADR 0049, B5), so the older row's declared handle is cleared, the daemon
/// logs the move once, and the wake reaches the newer row alone.
#[tokio::test]
async fn a_newer_join_moves_the_handle_and_only_the_newer_row_is_woken() {
    let _serial = SERIAL.lock().await;
    let mut row = start("cw2", None, false).await;
    let older = row.ws.clone();
    let newer = add_row(&mut row, "wake-row-2", HANDLE, "second").await;
    let id = row.next_id;
    row.next_id += 1;
    let payload = call(&mut row.conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    assert_eq!(find_row(&payload, &older).expect("older row")["agent_handle"], "", "the older row kept the handle");
    assert_eq!(find_row(&payload, &newer).expect("newer row")["agent_handle"], HANDLE);
    let moves = |log: &str| -> Vec<String> {
        log.lines().filter(|l| l.contains("agent.join: the handle moved off an older row")).map(str::to_string).collect()
    };
    wait_for("the move log line", || moves(&daemon_log(&row.env)).len() == 1).await;
    // The log is colored: drop the escape sequences so the fields read `name=value`.
    let line = moves(&daemon_log(&row.env)).remove(0);
    let line = {
        let mut plain = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                plain.push(c);
            }
        }
        plain
    };
    for part in [format!("from={older}"), format!("to={newer}"), format!("handle={HANDLE}")] {
        assert!(line.contains(&part), "the move line does not say {part}: {line}");
    }
    append_mail(&row.env, 1);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no row was woken");
    tokio::time::sleep(THREE_TICKS).await;
    assert_eq!(pings(&row.log), 1, "the wake did not reach exactly one row");
    assert!(!row.screen_has("[sot-comm]").await, "the older row was typed into");
    let id = row.next_id;
    row.next_id += 1;
    let res = call(&mut row.conn, id, op::PTY_SCREEN, serde_json::json!({ "workspace_id": newer })).await;
    assert!(
        res.payload["lines"].as_array().expect("lines").iter().any(|l| l.as_str().unwrap_or_default().contains("[sot-comm]")),
        "the newer row was not the one woken"
    );
    row.env.kill_daemon_bounded().await;
}

/// Two tomls declaring one handle (a crash or a failed save between a join's two saves): at boot the handle stays
/// only on the row the comm registry names as its last joiner, so the wake reaches that row alone.
#[tokio::test]
async fn two_tomls_on_one_handle_keep_it_on_the_last_joiner_at_boot() {
    let _serial = SERIAL.lock().await;
    let mut row = start("cwt", None, false).await;
    let second = add_row(&mut row, "wake-row-2", "wakeh2", "second").await;
    row.env.kill_daemon_bounded().await;
    let dir = row.env.app_config_dir().join(format!("workspaces-{TEST_STATE_HOST}"));
    let mut rewritten = 0;
    for entry in std::fs::read_dir(&dir).expect("read the toml dir").flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        if text.contains("\"wakeh2\"") {
            std::fs::write(entry.path(), text.replace("\"wakeh2\"", &format!("\"{HANDLE}\""))).unwrap();
            rewritten += 1;
        }
    }
    assert_eq!(rewritten, 1, "the second row's toml must be the one rewritten");
    set_entry(&row.env, serde_json::json!({ "host": TEST_STATE_HOST, "workspace_id": second }));
    row.env.spawn_sotd_with_prepended_path(&row.stub_dir);
    let (mut conn, mut next_id) = connect_and_hello(&row.env.socket_path).await;
    for ws in [&row.ws, &second] {
        poll_for_phase(&mut conn, &mut next_id, ws, "ready", BOUND.max(Duration::from_secs(60))).await;
    }
    let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    assert_eq!(find_row(&payload, &row.ws).expect("first row")["agent_handle"], "", "the older row kept the handle");
    assert_eq!(find_row(&payload, &second).expect("second row")["agent_handle"], HANDLE);
    let warning = "two row tomls declare one handle";
    let hits: Vec<String> = daemon_log(&row.env).lines().filter(|l| l.contains(warning)).map(str::to_string).collect();
    assert_eq!(hits.len(), 1, "the boot warning must appear once: {hits:?}");
    assert!(hits[0].contains(HANDLE), "the warning must name the handle: {}", hits[0]);
    // The harness's connection is the restarted daemon's: reuse it for the screens.
    row.conn = conn;
    row.next_id = next_id;
    append_mail(&row.env, 1);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no row was woken");
    tokio::time::sleep(THREE_TICKS).await;
    assert_eq!(pings(&row.log), 1, "the wake did not reach exactly one row");
    assert!(!row.screen_has("[sot-comm]").await, "the older row was typed into");
    let id = row.next_id;
    row.next_id += 1;
    let res = call(&mut row.conn, id, op::PTY_SCREEN, serde_json::json!({ "workspace_id": second })).await;
    assert!(
        res.payload["lines"].as_array().expect("lines").iter().any(|l| l.as_str().unwrap_or_default().contains("[sot-comm]")),
        "the second row was not the one woken"
    );
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

/// The daemon's own log.
fn daemon_log(env: &Env) -> String {
    std::fs::read_to_string(env.state_root.join("sot").join("sotd.log")).unwrap_or_default()
}

/// Waits, bounded by [`WAKE_WITHIN`], until `ready` holds; fails naming `what` otherwise.
async fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + WAKE_WITHIN;
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready(), "{what} not seen within {WAKE_WITHIN:?}");
}

/// [`wait_for`] for the stub's agents panel on the row's screen, which only an async read can see; the same
/// bound and the same failure shape. Sent before this, the mail could be read ahead of the panel's first draw.
async fn wait_for_panel(row: &mut Row) {
    let deadline = Instant::now() + WAKE_WITHIN;
    while Instant::now() < deadline {
        if row.screen_has("for agents").await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(row.screen_has("for agents").await, "the stub's panel not seen within {WAKE_WITHIN:?}");
}

/// The wake attempt's marks (`SOT_TEST_WAKE_MARKS`, see [`start_with`]).
fn marks(row: &Row) -> PathBuf {
    row.env._tmp.path().join("ctl").join("marks")
}

/// Focus moves to the agents panel during the hold while the rows through the box and the cursor stay put: the
/// final check reads the live screen, so it refuses and nothing is typed. Each guard is pinned by itself: the
/// attempt reached the hold, the final check did not pass, and the row's screen never held the wake line (the
/// Enter gate alone would also keep the ping away).
#[tokio::test]
async fn panel_focus_arriving_during_the_hold_is_refused() {
    let _serial = SERIAL.lock().await;
    let mut row = start_with("cwph", None, false, &["panel", "focus-hold"], &[]).await;
    wait_for_panel(&mut row).await;
    append_mail(&row.env, 1);
    let m = marks(&row);
    wait_for("the attempt's done mark", || m.join("done").exists()).await;
    assert!(m.join("focus-moved").exists(), "the stub never moved focus");
    assert!(m.join("hold").exists(), "no attempt reached the hold");
    assert!(!m.join("final-ok").exists(), "the final check passed with focus on the panel");
    assert!(!row.screen_has("[sot-comm]").await, "the wake line was typed into the row");
    assert_eq!(pings(&row.log), 0, "typed into a row whose focus had moved to the panel");
    row.env.kill_daemon_bounded().await;
}

/// Focus moves off the input box right after the final check passes: the line is typed, the live screen then
/// does not show it in main's box, so no Enter goes and the daemon logs it.
#[tokio::test]
async fn focus_moving_after_the_final_check_gets_no_enter() {
    let _serial = SERIAL.lock().await;
    let mut row = start_with("cwfa", None, false, &["panel", "focus-after", "echo-after=1"], &[]).await;
    wait_for_panel(&mut row).await;
    append_mail(&row.env, 1);
    let m = marks(&row);
    wait_for("the attempt's done mark", || m.join("done").exists()).await;
    assert!(m.join("focus-moved").exists(), "the stub never moved focus");
    wait_for("the warn line in the daemon log", || daemon_log(&row.env).contains("did not show in main's input box")).await;
    assert!(daemon_log(&row.env).contains("no Enter sent"), "the warn line does not say no Enter was sent");
    assert_eq!(pings(&row.log), 0, "sent Enter with focus off main's box");
    row.env.kill_daemon_bounded().await;
}

/// The typed line shows 0.65 s after the write, past the old 0.3 s quiet read, with 2 s of slack to OP_BUDGET: the wake waits for it, then sends
/// Enter in the same attempt. One ping, the line typed once, and no "left unsent" warning.
#[tokio::test]
async fn a_slow_echo_is_entered_in_the_same_attempt() {
    let _serial = SERIAL.lock().await;
    let row = start_with("cwse", None, false, &["echo-after=0.6"], &[]).await;
    append_mail(&row.env, 1);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?}");
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(pings(&row.log), 1);
    assert_eq!(others(&row.log), 0, "the wake line was typed more than once");
    assert!(!daemon_log(&row.env).contains("did not show in main's input box"), "the wake gave up on the line");
    row.env.kill_daemon_bounded().await;
}

/// The typed line shows 8 s after the write, past OP_BUDGET and two ticks: the first attempt types it and sends no
/// Enter, a later tick finds the box empty and does nothing, and the tick after the echo sends Enter alone. One ping,
/// the line typed once, the "did not show" warning once.
#[tokio::test]
async fn a_wake_line_left_unsent_is_completed_by_the_next_tick() {
    let _serial = SERIAL.lock().await;
    let row = start_with("cwlu", None, false, &["echo-after=8"], &[]).await;
    append_mail(&row.env, 1);
    assert!(wait_pings(&row.log, 1, WAKE_WITHIN).await, "no wake within {WAKE_WITHIN:?}");
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(pings(&row.log), 1);
    assert_eq!(others(&row.log), 0, "the wake line was typed more than once");
    let said = daemon_log(&row.env).matches("did not show in main's input box").count();
    assert_eq!(said, 1, "the left line should be named once in the daemon log");
    row.env.kill_daemon_bounded().await;
}
