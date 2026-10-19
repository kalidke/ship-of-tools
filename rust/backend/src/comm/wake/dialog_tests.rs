//! Tests of the dialog tick: what a screen and a registry entry call for, and the writes a dialog's open and close make
//! through the real `comm-status.sh`.

use super::*;
use serde_json::json;

const RULE: &str = "────────────────────────────────────────";

/// A row at rest: the main input box, its glyph followed by the NBSP.
fn free() -> Vec<String> {
    ["● Done.", "", RULE, "\u{276f}\u{a0}", RULE, "  ⏵⏵ auto mode on"].map(String::from).to_vec()
}

/// SYNTHETIC, after the wake's PERM_MENU (screen_tests.rs) with a footer: a permission dialog where the box was.
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
    writes.extend(tick(h, permission()));
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
