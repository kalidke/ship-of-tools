//! Tests of work-state: the reduction, who may write what, the one write through the registry lock, the read-clear and
//! the one read.

use super::*;
use serde_json::json;
use std::path::PathBuf;

const NOW: &str = "2026-10-21T00:00:00Z";

fn entry(v: Value) -> Map<String, Value> {
    v.as_object().expect("an object").clone()
}

/// `event` applied to `facts`; the entry after it, and whether it was to be written.
fn after(facts: Value, event: Event) -> (Map<String, Value>, bool) {
    let mut e = entry(facts);
    let write = apply_event(&mut e, &event, NOW).expect("not refused");
    (e, write)
}

fn shows(e: &Map<String, Value>) -> (&str, &str) {
    (e["state"].as_str().unwrap_or("?"), e["summary"].as_str().unwrap_or("?"))
}

fn declare(d: Declaration, text: &str) -> Event {
    Event::Declare(d, Some(text.to_string()))
}

/// The precedence: a dialog, then a question while no turn runs, then a turn, a wait, done, idle; each with its summary.
#[test]
fn the_reduction_follows_the_precedence() {
    let cases = [
        (json!({"dialog": "d", "question": "q", "floor": "user", "waiting": "w", "done": true, "note": "n"}), ("blocked", "d")),
        (json!({"question": "q", "waiting": "w", "done": true, "note": "n"}), ("blocked", "q")),
        (json!({"question": "q", "floor": "user", "note": "n"}), ("working", "n")),
        (json!({"floor": "machine", "waiting": "w", "note": "n"}), ("working", "n")),
        (json!({"waiting": "w", "done": true, "note": "n"}), ("waiting", "w")),
        (json!({"done": true, "note": "n"}), ("done", "n")),
        (json!({"note": "n"}), ("idle", "n")),
        (json!({}), ("idle", "")),
    ];
    for (facts, want) in cases {
        let mut e = entry(facts.clone());
        reduce(&mut e);
        assert_eq!(shows(&e), want, "{facts}");
    }
}

/// `blocked` keeps a wait, `waiting` clears a question, `working`, `idle` and `done` clear both; a question's or a
/// wait's text supersedes the older line.
#[test]
fn declarations_set_and_clear_their_facts() {
    let (e, _) = after(json!({"waiting": "job", "note": "old"}), declare(Declaration::Blocked, "which?"));
    assert_eq!((e["question"].as_str(), e["waiting"].as_str(), e.get("note")), (Some("which?"), Some("job"), None));
    assert_eq!(shows(&e), ("blocked", "which?"));
    let (e, _) = after(json!({"question": "which?"}), declare(Declaration::Waiting, "job"));
    assert!(e.get("question").is_none());
    assert_eq!(shows(&e), ("waiting", "job"));
    for d in [Declaration::Working, Declaration::Idle, Declaration::Done] {
        let (e, _) = after(json!({"question": "q", "waiting": "w"}), declare(d, "line"));
        assert!(e.get("question").is_none() && e.get("waiting").is_none(), "{d:?}");
        assert_eq!(e["note"].as_str(), Some("line"));
    }
    let (e, _) = after(json!({"done": true, "note": "kept"}), Event::Declare(Declaration::Working, None));
    assert!(e.get("done").is_none());
    assert_eq!(shows(&e), ("idle", "kept"), "no text keeps the last line");
    let (e, _) = after(json!({"note": "gone"}), declare(Declaration::Idle, ""));
    assert_eq!(shows(&e), ("idle", ""), "an empty text clears it");
}

/// A question or a wait with no text of its own is refused, and so is a dialog with no text; with its own text kept,
/// a textless `blocked` is accepted.
#[test]
fn a_textless_question_wait_or_dialog_is_refused() {
    for d in [Declaration::Blocked, Declaration::Waiting] {
        for text in [None, Some(String::new())] {
            let mut e = entry(json!({"floor": "user", "question": "\n"}));
            if d == Declaration::Waiting {
                e.insert("waiting".into(), json!(""));
            }
            let before = e.clone();
            assert_eq!(apply_event(&mut e, &Event::Declare(d, text), NOW), Err(Refused), "{d:?}");
            assert_eq!(e, before, "a refusal changes nothing");
        }
    }
    assert_eq!(apply_event(&mut entry(json!({})), &Event::Dialog(String::new()), NOW), Err(Refused));
    let (e, _) = after(json!({"question": "still open"}), Event::Declare(Declaration::Blocked, None));
    assert_eq!(shows(&e), ("blocked", "still open"));
}

/// A person's prompt answers a question and reads a result; a machine's clears nothing; either ends a dialog.
#[test]
fn a_prompt_sets_the_floor_and_a_persons_answers() {
    let facts = json!({"question": "q", "done": true, "dialog": "d"});
    let (e, _) = after(facts.clone(), Event::Prompt(Origin::User));
    assert_eq!((e["floor"].as_str(), e.get("question"), e.get("done"), e.get("dialog")), (Some("user"), None, None, None));
    let (e, _) = after(facts, Event::Prompt(Origin::Machine));
    assert_eq!((e["floor"].as_str(), e["question"].as_str(), e.get("dialog")), (Some("machine"), Some("q"), None));
    assert_eq!(shows(&e).0, "working");
}

/// A stop leaves `done` only after a person's turn with nothing pending, and takes the floor, the wake's mark and any
/// dialog with it.
#[test]
fn a_stop_ends_the_turn() {
    let (e, _) = after(json!({"floor": "user", "stop_at": "t", "dialog": "d"}), Event::Stop);
    assert_eq!((e.get("floor"), e.get("stop_at"), e.get("dialog"), e.get("done")), (None, None, None, Some(&json!(true))));
    for pending in [json!({"floor": "user", "question": "q"}), json!({"floor": "user", "waiting": "w"}), json!({"floor": "machine"})] {
        let (e, _) = after(pending.clone(), Event::Stop);
        assert!(e.get("done").is_none(), "{pending}");
    }
}

/// Every write but a tick and a read restamps the row's time and drops the legacy keys.
#[test]
fn a_write_restamps_and_drops_legacy_keys() {
    let (e, write) = after(json!({"turn_origin": "x", "sticky": true, "sticky_at": "t"}), Event::Resumed);
    assert!(write);
    assert_eq!((e["status_at"].as_str(), e["last_seen"].as_str()), (Some(NOW), Some(NOW)));
    assert!(e.get("turn_origin").is_none() && e.get("sticky").is_none() && e.get("sticky_at").is_none());
}

/// The heartbeat restamps a running turn's time and sets no fact; with no turn it writes nothing.
#[test]
fn a_tick_restamps_only_a_running_turn() {
    let (e, write) = after(json!({"floor": "user", "state": "working", "status_at": "old"}), Event::Tick);
    assert!(write);
    assert_eq!((e["status_at"].as_str(), e["state"].as_str()), (Some(NOW), Some("working")));
    let (e, write) = after(json!({"state": "idle", "status_at": "old"}), Event::Tick);
    assert!(!write);
    assert_eq!(e["status_at"].as_str(), Some("old"));
}

/// A person's read clears `done`, shows what the other facts say, and restamps nothing; with no `done`, nothing.
#[test]
fn a_read_clears_done_and_restamps_nothing() {
    let (e, write) = after(json!({"state": "done", "done": true, "summary": "s", "status_at": "old"}), Event::Read);
    assert!(write);
    assert_eq!((e["state"].as_str(), e["summary"].as_str(), e["status_at"].as_str()), (Some("idle"), Some("s"), Some("old")));
    let (e, _) = after(json!({"state": "waiting", "waiting": "job", "done": true}), Event::Read);
    assert_eq!((e["state"].as_str(), e.get("done")), (Some("waiting"), None), "a wait still outranks");
    let (_, write) = after(json!({"state": "idle"}), Event::Read);
    assert!(!write);
}

/// A registry under a temporary comm home, named by `SOT_COMM_HOME` while the guards live; the host is `host-4`.
struct Home {
    _serial: std::sync::MutexGuard<'static, ()>,
    _env: [crate::paths::EnvGuard; 2],
    dir: tempfile::TempDir,
}

impl Home {
    fn with(agents: Value) -> Home {
        let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = ["SOT_COMM_HOME", "SOT_SELF_HOST"].map(crate::paths::EnvGuard::capture);
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("SOT_COMM_HOME", dir.path());
        std::env::set_var("SOT_SELF_HOST", "host-4");
        std::fs::write(dir.path().join("registry.json"), json!({ "agents": agents }).to_string()).expect("registry");
        Home { _serial: serial, _env: env, dir }
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.dir.path().join("registry.json")).expect("registry")
    }

    fn agent(&self, h: &str) -> Value {
        serde_json::from_slice::<Value>(&self.bytes()).expect("json")["agents"][h].clone()
    }
}

/// The one write: a missing entry is nothing to stamp, a stamp lands under the lock, a refusal writes nothing.
#[test]
fn a_stamp_writes_one_entry_under_the_lock() {
    let home = Home::with(json!({"h": {"host": "host-4", "state": "idle"}}));
    assert_eq!(write_event("absent", &Event::Stop, Duration::from_secs(1)), Ok(Stamped::NoEntry));
    assert_eq!(write_event("h", &Event::Prompt(Origin::User), Duration::from_secs(1)), Ok(Stamped::Written));
    assert_eq!(home.agent("h")["state"], "working");
    let before = home.bytes();
    assert_eq!(write_event("h", &Event::Declare(Declaration::Waiting, None), Duration::from_secs(1)), Err(StampError::Refused));
    assert_eq!(home.bytes(), before);
}

/// A held lock fails closed: nothing written, and the reason names the lock.
#[test]
fn a_held_lock_writes_nothing() {
    let home = Home::with(json!({"h": {"host": "host-4", "state": "idle"}}));
    std::fs::write(home.dir.path().join(".registry.lock"), "someone:-:-:-:1:-\n").expect("lock");
    let before = home.bytes();
    assert!(matches!(write_event("h", &Event::Stop, Duration::from_millis(100)), Err(StampError::Registry(EditError::Locked(_)))));
    assert_eq!(home.bytes(), before);
}

fn row(label: &str, name: &str, declared: &str) -> Arc<Workspace> {
    let ws = Workspace::from_label(label, PathBuf::from("/p"), false, "claude".into(), name.into(), String::new());
    *ws.agent_handle.lock().unwrap() = declared.to_string();
    Arc::new(ws)
}

/// A person's read clears the row's own entry through its bound handle, keeps its summary and time, and never touches
/// another host's same-named entry.
#[test]
fn a_read_clears_the_rows_own_entry_on_this_host() {
    let home = Home::with(json!({
        "declared": {"host": "host-4", "state": "done", "done": true, "summary": "s", "status_at": "t"},
        "named": {"host": "hostB", "state": "done", "done": true},
    }));
    let declared = row("a", "", "declared");
    read_clear(&declared, &[declared.clone()], "host-4");
    let e = home.agent("declared");
    assert_eq!((e["state"].as_str(), e.get("done"), e["summary"].as_str(), e["status_at"].as_str()), (Some("idle"), None, Some("s"), Some("t")));
    let before = home.bytes();
    let named = row("b", "named", "");
    read_clear(&named, &[named.clone()], "host-4");
    assert_eq!(home.bytes(), before, "another host's entry is not this row's");
}

/// The one read: through the bound handle, from this host's entry only; a stored name another row declares binds
/// nothing.
#[test]
fn shown_reads_the_bound_handle_on_this_host() {
    let agents = json!({
        "mine": {"host": "host-4", "state": "working", "summary": "s", "status_at": "t"},
        "theirs": {"host": "hostB", "state": "blocked"},
    });
    let mine = row("a", "mine", "");
    let shown_mine = shown_of(&mine, &[mine.clone()], Some(&agents), "host-4");
    assert_eq!(shown_mine, Shown { state: "working".into(), summary: "s".into(), status_at: "t".into() });
    let theirs = row("b", "theirs", "");
    assert_eq!(shown_of(&theirs, &[theirs.clone()], Some(&agents), "host-4"), Shown::default());
    let other = row("c", "", "mine");
    assert_eq!(shown_of(&mine, &[mine.clone(), other], Some(&agents), "host-4"), Shown::default(), "another row declares it");
}

/// The read-decide-write is one critical section: a stamp waiting on the lock decides against the row as it is once it
/// holds the lock, never against what it might have read before. A `done` committed while a `stop` waits is kept, and a
/// machine start committed while it waits ends the turn gray, not blue. The holder lets the stamp start before it
/// writes: if the stamp has not reached the lock by then, the case cannot fail, only pass trivially.
#[test]
fn a_stamp_decides_against_the_row_under_the_lock() {
    for (seed, competing, want) in [
        (json!({"host": "host-4"}), ("done", json!(true)), "done"),
        (json!({"host": "host-4", "floor": "user"}), ("floor", json!("machine")), "idle"),
    ] {
        let home = Home::with(json!({ "h": seed }));
        let lock = home.dir.path().join(".registry.lock");
        let held = crate::comm::registry::lock::acquire(&lock, Duration::from_secs(1)).expect("the holder takes the lock");
        let stop = std::thread::spawn(|| write_event("h", &Event::Stop, Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(200));
        let path = home.dir.path().join("registry.json");
        let mut root: Value = serde_json::from_slice(&std::fs::read(&path).expect("registry")).expect("json");
        root["agents"]["h"][competing.0] = competing.1.clone();
        std::fs::write(&path, root.to_string()).expect("the competing write");
        drop(held);
        assert_eq!(stop.join().expect("the stamp"), Ok(Stamped::Written));
        assert_eq!(home.agent("h")["state"], want, "{}", competing.0);
    }
}
