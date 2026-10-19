//! Tests of the dialog tick: what a screen and a registry entry call for, and the writes a dialog's open and close make
//! through the real `comm-status.sh`.

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

/// Only a working row, or one whose fact the tick must clear, is read at all.
#[test]
fn only_working_rows_and_rows_with_the_fact_are_due() {
    assert!(due(&json!({"floor": "user", "state": "working"})));
    assert!(due(&json!({"state": "blocked", "dialog": "claude dialog open"})));
    for at_rest in ["idle", "done", "waiting"] {
        assert!(!due(&json!({"state": at_rest})), "{at_rest}");
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

/// A row at rest is never attached, and a fact left on a row is still read and cleared: with three rows at rest (idle,
/// done, a question) and one holding the fact, the sweep reads only that one and writes only its `resumed`.
#[test]
fn a_row_at_rest_is_never_read_and_a_left_fact_still_clears() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = json!({"agents": {
        "idle": {"state": "idle"},
        "done": {"state": "done", "done": true},
        "asked": {"state": "blocked", "question": "Which approach?"},
        "left": {"state": "blocked", "dialog": "claude dialog open"},
    }});
    std::fs::write(dir.path().join("registry.json"), registry.to_string()).expect("registry");
    let rows: Vec<(String, PathBuf)> = ["idle", "done", "asked", "left"].iter().map(|h| (h.to_string(), PathBuf::from(h))).collect();
    let (reads, writes) = (std::cell::RefCell::new(Vec::new()), std::cell::RefCell::new(Vec::new()));
    let read = |d: &Path| {
        reads.borrow_mut().push(d.to_path_buf());
        // Every screen shows a dialog, as a menu a person opened at rest would: a read of a row at rest would write.
        Some(permission())
    };
    let write = |_: &Path, h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    check_rows(dir.path(), &rows, &read, &write, &mut HashSet::new());
    assert_eq!(reads.into_inner(), vec![PathBuf::from("left")]);
    assert_eq!(writes.into_inner(), vec![], "the left fact's dialog still shows: nothing to write");
    let free_read = |_: &Path| Some(free());
    let writes = std::cell::RefCell::new(Vec::new());
    let write = |_: &Path, h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    check_rows(dir.path(), &rows, &free_read, &write, &mut HashSet::new());
    assert_eq!(writes.into_inner(), vec![("left".to_string(), Stamp::Resumed)]);
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

/// A comm home whose bin holds the tree's `comm-status.sh` and its library, and one row, `h`, with `entry`.
#[cfg(unix)]
fn home_with(entry: serde_json::Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let comm = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../comm");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let mut sources = vec![comm.join("work_state/comm-status.sh")];
    for f in std::fs::read_dir(comm.join("lib")).expect("lib") {
        let p = f.expect("entry").path();
        if p.extension().is_some_and(|e| e == "sh") {
            sources.push(p);
        }
    }
    for src in sources {
        std::os::unix::fs::symlink(&src, bin.join(src.file_name().expect("name"))).expect("symlink");
    }
    std::fs::write(dir.path().join("registry.json"), json!({"agents": {"h": entry}}).to_string()).expect("registry");
    dir
}

#[cfg(unix)]
fn row(home: &Path) -> serde_json::Value {
    let bytes = std::fs::read(home.join("registry.json")).expect("registry");
    serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")["agents"]["h"].clone()
}

/// One tick of [`check_rows`] over row `h` showing `screen`, writing through the real script; the writes it made.
#[cfg(unix)]
fn tick(home: &Path, screen: Vec<String>) -> Vec<Stamp> {
    let writes = std::cell::RefCell::new(Vec::new());
    let write = |h: &Path, handle: &str, s: &Stamp| {
        writes.borrow_mut().push(s.clone());
        stamp(h, handle, s)
    };
    let rows = [("h".to_string(), PathBuf::from("unused"))];
    let mut failing = HashSet::new();
    check_rows(home, &rows, &|_: &Path| Some(screen.clone()), &write, &mut failing);
    assert!(failing.is_empty(), "a write failed");
    writes.into_inner()
}

/// The dialog opens, stays and closes: exactly one `dialog` and one `resumed`, each reduced by the script. Fails if
/// the tick writes on every tick, misses either edge, or the script's entry does not stamp the named row.
#[cfg(unix)]
#[test]
fn the_open_then_close_writes_one_dialog_and_one_resumed() {
    let home = home_with(json!({"floor": "user", "state": "working", "summary": ""}));
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
    assert!(!h.join("state").exists() || std::fs::read_dir(h.join("state")).expect("state").all(|f| {
        !f.expect("entry").file_name().to_string_lossy().starts_with("dialog-")
    }), "the daemon's entry writes no marker");
}

/// After a restart the first tick clears a fact whose dialog has gone; nothing is kept between ticks.
#[cfg(unix)]
#[test]
fn a_fact_left_by_a_former_run_clears_on_the_first_tick() {
    let home = home_with(json!({"floor": "user", "state": "blocked", "dialog": "claude dialog open", "summary": "claude dialog open"}));
    assert_eq!(tick(home.path(), free()), vec![Stamp::Resumed]);
    assert_eq!(row(home.path())["state"].as_str(), Some("working"));
}
