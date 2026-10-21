//! Tests of the dialog tick: what a screen and a registry entry call for, and the writes a dialog's open and close make
//! through work-state's one write.

use super::*;
use serde_json::json;

const RULE: &str = "────────────────────────────────────────";

/// A row at rest: the main input box, its glyph followed by the NBSP.
fn free() -> Vec<String> {
    ["● Done.", "", RULE, "\u{276f}\u{a0}", RULE, "  ⏵⏵ auto mode on"].map(String::from).to_vec()
}

/// SYNTHETIC, after the wake's PERM_MENU (screen_tests.rs) with a real permission footer, where the box was.
fn permission() -> Vec<String> {
    [
        "● Bash(rm -rf build)",
        RULE,
        " Bash command",
        "",
        "   rm -rf build",
        "   Remove the build directory",
        "",
        " Do you want to proceed?",
        " \u{276f} 1. Yes",
        "   2. Yes, and don't ask again for rm commands",
        "   3. No, and tell Claude what to do differently (esc)",
        "",
        " Esc to cancel \u{b7} Tab to amend",
    ]
    .map(String::from)
    .to_vec()
}

#[test]
fn a_write_only_where_screen_and_registry_disagree() {
    let shown = || Some("claude dialog: Do you want to proceed?".to_string());
    let working = json!({"floor": "user", "state": "working"});
    let red = json!({"floor": "user", "state": "blocked", "dialog": "claude dialog: Do you want to proceed?"});
    assert_eq!(stamp_for(shown(), &working), Some(Stamp::Dialog("claude dialog: Do you want to proceed?".into())));
    assert_eq!(stamp_for(shown(), &red), None, "already set");
    assert_eq!(stamp_for(None, &red), Some(Stamp::Resumed));
    assert_eq!(stamp_for(None, &working), None, "nothing to clear");
}

/// Every row is read but one red by its question with no dialog fact, which keeps its question as its summary.
#[test]
fn every_row_but_one_red_by_its_question_is_due() {
    assert!(due(&json!({"floor": "user", "state": "working"})));
    assert!(due(&json!({"state": "blocked", "dialog": "claude dialog open"})));
    for at_rest in ["idle", "done", "waiting"] {
        assert!(due(&json!({"state": at_rest})), "{at_rest}");
    }
    // An AskUserQuestion menu: red by the question its hook wrote before the menu drew, and it keeps that summary.
    assert!(!due(&json!({"state": "blocked", "question": "Which approach?", "summary": "Which approach?"})));
}

/// The same dialog with its text changed, as a menu's tab advances or auto mode's countdown ticks.
fn permission_changed() -> Vec<String> {
    let mut screen = permission();
    screen[7] = " Do you want to make this edit to build.rs?".to_string();
    screen.insert(12, " Claude Code will automatically deny this request in 0:41".to_string());
    screen
}

/// A row at rest is read like a working one (a background agent's dialog after its lead's Stop waits on the person), a
/// row red by its question is not, and a fact left on a row is read and cleared: with rows idle, done, asked and left
/// all showing a dialog, the sweep reads all but the asked row and writes the dialog on the two at rest; with every
/// screen free it writes only the left row's `resumed`.
#[test]
fn a_row_at_rest_is_read_a_question_row_is_not_and_a_left_fact_clears() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = json!({"agents": {
        "idle": {"state": "idle"},
        "done": {"state": "done", "done": true},
        "asked": {"state": "blocked", "question": "Which approach?"},
        "left": {"state": "blocked", "dialog": "claude dialog: Do you want to proceed?"},
    }});
    std::fs::write(dir.path().join("registry.json"), registry.to_string()).expect("registry");
    let rows: Vec<(String, PathBuf)> = ["idle", "done", "asked", "left"].iter().map(|h| (h.to_string(), PathBuf::from(h))).collect();
    let (reads, writes) = (std::cell::RefCell::new(Vec::new()), std::cell::RefCell::new(Vec::new()));
    let read = |d: &Path| {
        reads.borrow_mut().push(d.to_path_buf());
        Seen::Screen(permission())
    };
    let write = |h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    check_rows(dir.path(), &rows, &read, &write, &mut HashSet::new());
    assert_eq!(reads.into_inner(), ["idle", "done", "left"].map(PathBuf::from).to_vec());
    let open = Stamp::Dialog("claude dialog: Do you want to proceed?".into());
    assert_eq!(writes.into_inner(), vec![("idle".to_string(), open.clone()), ("done".to_string(), open)]);
    let free_read = |_: &Path| Seen::Screen(free());
    let writes = std::cell::RefCell::new(Vec::new());
    let write = |h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    check_rows(dir.path(), &rows, &free_read, &write, &mut HashSet::new());
    assert_eq!(writes.into_inner(), vec![("left".to_string(), Stamp::Resumed)]);
}

/// A leg the probe shows ended has no dialog any more, so its fact clears; a row that does not answer keeps it.
#[test]
fn an_ended_leg_clears_its_fact_and_an_unreachable_one_keeps_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = json!({"agents": {"gone": {"state": "blocked", "dialog": "x"}, "mute": {"state": "blocked", "dialog": "x"}}});
    std::fs::write(dir.path().join("registry.json"), registry.to_string()).expect("registry");
    let rows: Vec<(String, PathBuf)> = ["gone", "mute"].iter().map(|h| (h.to_string(), PathBuf::from(h))).collect();
    let read = |d: &Path| if d == Path::new("gone") { Seen::Ended } else { Seen::Unknown };
    let writes = std::cell::RefCell::new(Vec::new());
    let write = |h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    check_rows(dir.path(), &rows, &read, &write, &mut HashSet::new());
    assert_eq!(writes.into_inner(), vec![("gone".to_string(), Stamp::Resumed)]);
}

/// Pacing: a sweep that runs past the period (30 s on a 5 s tick) is never overlapped, and the ticks it missed are
/// not queued behind it: one more sweep starts when it ends, then the 5 s grid resumes. Fails with tokio's default
/// `Burst`, which runs the six missed ticks back to back.
#[tokio::test(start_paused = true)]
async fn a_long_sweep_never_stacks_another_behind_it() {
    let (running, starts) = (std::rc::Rc::new(std::cell::Cell::new(false)), std::rc::Rc::new(std::cell::RefCell::new(Vec::new())));
    let t0 = tokio::time::Instant::now();
    let sweeps = {
        let (running, starts) = (running.clone(), starts.clone());
        every(Duration::from_secs(5), move || {
            let (running, starts) = (running.clone(), starts.clone());
            async move {
                assert!(!running.get(), "a sweep started while another ran");
                running.set(true);
                let first = starts.borrow().is_empty();
                starts.borrow_mut().push((tokio::time::Instant::now() - t0).as_secs());
                if first {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                running.set(false);
            }
        })
    };
    let _ = tokio::time::timeout(Duration::from_secs(41), sweeps).await;
    assert_eq!(*starts.borrow(), vec![0, 30, 35, 40]);
}

/// A comm home whose registry holds one row, `h`, with `entry`, named by `SOT_COMM_HOME` while the guard lives.
fn home_with(entry: serde_json::Value) -> (std::sync::MutexGuard<'static, ()>, crate::paths::EnvGuard, tempfile::TempDir) {
    let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = crate::paths::EnvGuard::capture("SOT_COMM_HOME");
    let dir = tempfile::tempdir().expect("tempdir");
    std::env::set_var("SOT_COMM_HOME", dir.path());
    std::fs::write(dir.path().join("registry.json"), json!({"agents": {"h": entry}}).to_string()).expect("registry");
    (serial, env, dir)
}

fn row(home: &Path) -> serde_json::Value {
    let bytes = std::fs::read(home.join("registry.json")).expect("registry");
    serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")["agents"]["h"].clone()
}

/// One tick of [`check_rows`] over row `h` showing `screen`, writing through the tick's own write; the writes it made.
fn tick(home: &Path, screen: Vec<String>) -> Vec<Stamp> {
    let writes = std::cell::RefCell::new(Vec::new());
    let record = |handle: &str, s: &Stamp| {
        writes.borrow_mut().push(s.clone());
        stamp_dialog(handle, s)
    };
    let rows = [("h".to_string(), PathBuf::from("unused"))];
    let mut failing = HashSet::new();
    check_rows(home, &rows, &|_: &Path| Seen::Screen(screen.clone()), &record, &mut failing);
    assert!(failing.is_empty(), "a write failed");
    writes.into_inner()
}

/// The dialog opens, stays and closes: exactly one `dialog` and one `resumed`, each reduced by work-state's write.
/// Fails if the tick writes on every tick, misses either edge, or the write does not stamp the named row.
#[test]
fn the_open_then_close_writes_one_dialog_and_one_resumed() {
    let (_serial, _env, home) = home_with(json!({"floor": "user", "state": "working", "summary": ""}));
    let h = home.path();
    let mut writes = Vec::new();
    writes.extend(tick(h, free()));
    writes.extend(tick(h, permission()));
    assert_eq!((row(h)["state"].as_str(), row(h)["summary"].as_str()), (Some("blocked"), Some("claude dialog: Do you want to proceed?")));
    // The text changes while the dialog stays open (a tab answered, a countdown): keyed by row, nothing new is written.
    writes.extend(tick(h, permission_changed()));
    writes.extend(tick(h, free()));
    assert_eq!(row(h)["state"].as_str(), Some("working"));
    assert!(row(h).get("dialog").is_none());
    writes.extend(tick(h, free()));
    assert_eq!(writes, vec![Stamp::Dialog("claude dialog: Do you want to proceed?".into()), Stamp::Resumed]);
    assert!(!h.join("state").exists(), "the daemon's write leaves no marker");
}

/// A row at rest whose screen shows a dialog (a background agent's permission prompt after its lead's Stop) reads red,
/// and reads idle again when it closes.
#[test]
fn a_dialog_on_a_row_at_rest_reads_red_then_idle() {
    let (_serial, _env, home) = home_with(json!({"state": "idle", "summary": "ready", "note": "ready"}));
    let h = home.path();
    assert_eq!(tick(h, permission()), vec![Stamp::Dialog("claude dialog: Do you want to proceed?".into())]);
    assert_eq!((row(h)["state"].as_str(), row(h)["summary"].as_str()), (Some("blocked"), Some("claude dialog: Do you want to proceed?")));
    assert_eq!(tick(h, free()), vec![Stamp::Resumed]);
    assert_eq!((row(h)["state"].as_str(), row(h)["summary"].as_str()), (Some("idle"), Some("ready")));
}

/// After a restart the first tick clears a fact whose dialog has gone; nothing is kept between ticks.
#[test]
fn a_fact_left_by_a_former_run_clears_on_the_first_tick() {
    let (_serial, _env, home) =
        home_with(json!({"floor": "user", "state": "blocked", "dialog": "claude dialog open", "summary": "claude dialog open"}));
    assert_eq!(tick(home.path(), free()), vec![Stamp::Resumed]);
    assert_eq!(row(home.path())["state"].as_str(), Some("working"));
}
