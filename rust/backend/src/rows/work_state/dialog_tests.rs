//! Tests of the dialog tick: what a screen and a registry entry call for, and the writes a dialog's open and close make
//! through work-state's one write.

use super::*;
use serde_json::json;

/// A row with an entry under `handle` and no leg to watch.
fn plain(handle: &str) -> Row {
    Row { handle: handle.to_string(), state_dir: PathBuf::from(handle), trust: None }
}

fn no_trust(_: &str, _: u64, _: Startup) -> bool {
    false
}

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
    let rows: Vec<Row> = ["idle", "done", "asked", "left"].iter().map(|h| plain(h)).collect();
    let (reads, writes) = (std::cell::RefCell::new(Vec::new()), std::cell::RefCell::new(Vec::new()));
    let read = |d: &Path| {
        reads.borrow_mut().push(d.to_path_buf());
        Seen::Screen(permission())
    };
    let write = |h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    check_rows(dir.path(), &rows, &read, &write, &no_trust, &mut Runs::default());
    assert_eq!(reads.into_inner(), ["idle", "done", "left"].map(PathBuf::from).to_vec());
    let open = Stamp::Dialog("claude dialog: Do you want to proceed?".into());
    assert_eq!(writes.into_inner(), vec![("idle".to_string(), open.clone()), ("done".to_string(), open)]);
    let free_read = |_: &Path| Seen::Screen(free());
    let writes = std::cell::RefCell::new(Vec::new());
    let write = |h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    check_rows(dir.path(), &rows, &free_read, &write, &no_trust, &mut Runs::default());
    assert_eq!(writes.into_inner(), vec![("left".to_string(), Stamp::Resumed)]);
}

/// A leg the probe shows ended has no dialog any more, so its fact clears; a row that does not answer keeps it.
#[test]
fn an_ended_leg_clears_its_fact_and_an_unreachable_one_keeps_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = json!({"agents": {"gone": {"state": "blocked", "dialog": "x"}, "mute": {"state": "blocked", "dialog": "x"}}});
    std::fs::write(dir.path().join("registry.json"), registry.to_string()).expect("registry");
    let rows: Vec<Row> = ["gone", "mute"].iter().map(|h| plain(h)).collect();
    let read = |d: &Path| if d == Path::new("gone") { Seen::Ended } else { Seen::Unknown("phase starting".to_string()) };
    let writes = std::cell::RefCell::new(Vec::new());
    let write = |h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    check_rows(dir.path(), &rows, &read, &write, &no_trust, &mut Runs::default());
    assert_eq!(writes.into_inner(), vec![("gone".to_string(), Stamp::Resumed)]);
}

/// A due row whose screen was not read is told once per run, with its reason, and its run ends at the next read that
/// found its screen or its end; an unreadable registry is told once per run too. Fails if a run is not kept between
/// ticks (the log would repeat every 5 s) or is never ended (the next run would go untold).
#[test]
fn a_row_not_read_and_an_unreadable_registry_are_each_told_once_per_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rows = [plain("h")];
    let write = |_: &str, _: &Stamp| Ok(());
    let mut runs = Runs::default();
    check_rows(dir.path(), &rows, &|_: &Path| Seen::Ended, &write, &no_trust, &mut runs);
    assert!(runs.registry, "an unreadable registry was not held as a run");
    std::fs::write(dir.path().join("registry.json"), json!({"agents": {"h": {"state": "working"}}}).to_string()).expect("registry");
    let mute = |_: &Path| Seen::Unknown("attach: refused".to_string());
    check_rows(dir.path(), &rows, &mute, &write, &no_trust, &mut runs);
    assert!(!runs.registry, "a registry read did not end its run");
    assert!(runs.unread.contains("h"), "a row not read was not held as a run");
    check_rows(dir.path(), &rows, &|_: &Path| Seen::Screen(free()), &write, &no_trust, &mut runs);
    assert!(runs.unread.is_empty(), "a screen read did not end the row's run");
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
    let rows = [plain("h")];
    let mut runs = Runs::default();
    check_rows(home, &rows, &|_: &Path| Seen::Screen(screen.clone()), &record, &no_trust, &mut runs);
    assert!(runs.failing.is_empty(), "a write failed");
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

/// A Ready screen can need its person's login before the session ever joins. The ordinary tick writes the dialog
/// fact, making the row red and visible to the Queue; removing the notice clears the fact on the next tick.
#[test]
fn a_not_logged_in_row_gets_the_dialog_stamp_and_reads_red() {
    let (_serial, _env, home) = home_with(json!({"state": "idle", "summary": "ready", "note": "ready"}));
    let h = home.path();
    let mut screen: Vec<String> = include_str!("../../../tests/fixtures/comm_wake/linux-not-logged-in.txt")
        .lines()
        .map(|l| if l == "\u{276f}" { "\u{276f}\u{a0}".to_string() } else { l.to_string() })
        .collect();
    let summary = "claude: not logged in; run /login in this row";
    assert_eq!(tick(h, screen.clone()), vec![Stamp::Dialog(summary.into())]);
    let entry = row(h);
    assert_eq!((entry["state"].as_str(), entry["dialog"].as_str(), entry["summary"].as_str()), (Some("blocked"), Some(summary), Some(summary)));
    screen.retain(|l| !l.contains("Not logged in · Run /login"));
    assert_eq!(tick(h, screen), vec![Stamp::Resumed]);
    let entry = row(h);
    assert_eq!(entry["state"].as_str(), Some("idle"));
    assert!(entry.get("dialog").is_none());
}

/// After a restart the first tick clears a fact whose dialog has gone; nothing is kept between ticks.
#[test]
fn a_fact_left_by_a_former_run_clears_on_the_first_tick() {
    let (_serial, _env, home) =
        home_with(json!({"floor": "user", "state": "blocked", "dialog": "claude dialog open", "summary": "claude dialog open"}));
    assert_eq!(tick(home.path(), free()), vec![Stamp::Resumed]);
    assert_eq!(row(home.path())["state"].as_str(), Some("working"));
}

/// The Linux capture of the folder-trust dialog, as the wake reads it.
fn trust_dialog() -> Vec<String> {
    include_str!("../../../tests/fixtures/comm_wake/linux-trust-dialog.txt").lines().map(String::from).collect()
}

/// A trust dialog on a row with a started leg goes to the restart, and its stamp is not written (the screen is about
/// to be gone); the same screen on a row with no leg is stamped as any dialog is. Fails if a leg's dialog is stamped
/// red or the restart is asked of a row with no leg.
#[test]
fn a_trust_dialog_on_an_open_leg_goes_to_the_restart_and_is_not_stamped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = json!({"agents": {"leg": {"state": "working"}, "plain": {"state": "working"}}});
    std::fs::write(dir.path().join("registry.json"), registry.to_string()).expect("registry");
    let mut rows = vec![plain("leg"), plain("plain")];
    rows[0].trust = Some(("ws-leg".to_string(), 4));
    let (asked, writes) = (std::cell::RefCell::new(Vec::new()), std::cell::RefCell::new(Vec::new()));
    let trust = |id: &str, generation: u64, startup: Startup| {
        asked.borrow_mut().push((id.to_string(), generation, startup));
        true
    };
    let write = |h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    check_rows(dir.path(), &rows, &|_: &Path| Seen::Screen(trust_dialog()), &write, &trust, &mut Runs::default());
    assert_eq!(asked.into_inner(), vec![("ws-leg".to_string(), 4, Startup::TrustDialog)]);
    assert_eq!(writes.into_inner(), vec![("plain".to_string(), Stamp::Dialog("claude dialog open".into()))]);
}

/// A row with an open leg is read whatever its handle, entry or question: with no handle, with a handle the registry
/// does not hold, and red by an AskUserQuestion menu; a row red by its question with no leg is still not read.
#[test]
fn an_open_leg_is_read_with_no_handle_or_red_by_its_question() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = json!({"agents": {"asked": {"state": "blocked", "question": "Which approach?"}, "asked-no-leg": {"state": "blocked", "question": "Which?"}}});
    std::fs::write(dir.path().join("registry.json"), registry.to_string()).expect("registry");
    let leg = |handle: &str, id: &str| Row { trust: Some((id.to_string(), 1)), ..plain(handle) };
    let rows = vec![leg("", "ws-a"), leg("absent", "ws-b"), leg("asked", "ws-c"), plain("asked-no-leg")];
    let rows: Vec<Row> = rows
        .into_iter()
        .map(|mut r| {
            r.state_dir = PathBuf::from(if r.handle.is_empty() { "no-handle".to_string() } else { r.handle.clone() });
            r
        })
        .collect();
    let (reads, asked) = (std::cell::RefCell::new(Vec::new()), std::cell::RefCell::new(Vec::new()));
    let read = |d: &Path| {
        reads.borrow_mut().push(d.to_path_buf());
        Seen::Screen(free())
    };
    let trust = |id: &str, _: u64, startup: Startup| {
        asked.borrow_mut().push((id.to_string(), startup));
        false
    };
    let write = |_: &str, _: &Stamp| -> Result<(), String> { panic!("a free screen with no fact writes nothing") };
    check_rows(dir.path(), &rows, &read, &write, &trust, &mut Runs::default());
    assert_eq!(*reads.borrow(), ["no-handle", "absent", "asked"].map(PathBuf::from).to_vec());
    let main = |id: &str| (id.to_string(), Startup::MainBox);
    assert_eq!(*asked.borrow(), vec![main("ws-a"), main("ws-b"), main("ws-c")]);

    // No registry at all: a row with an entry is not read, one with a leg still is.
    std::fs::remove_file(dir.path().join("registry.json")).expect("remove registry");
    reads.borrow_mut().clear();
    check_rows(dir.path(), &rows, &read, &write, &trust, &mut Runs::default());
    assert_eq!(reads.into_inner(), ["no-handle", "absent", "asked"].map(PathBuf::from).to_vec());
}

/// The main box closes the leg record at the generation read (and only that one), so the row is read no more; a leg
/// is read only while the row's phase is `ready`. Fails if the seam ignores the main box or its generation.
#[test]
fn the_main_box_closes_the_trust_leg() {
    use crate::rows::workspace::{Observation, SupervisorIdentity};
    let workspaces = Workspaces::new();
    let mut row = crate::rows::Workspace::from_label(
        "main-box",
        PathBuf::from("/tmp/sot-main-box"),
        false,
        "claude".to_string(),
        String::new(),
        String::new(),
    );
    row.runtime = "capsule".to_string();
    let ws = workspaces.insert(row);
    let g = ws.begin_spawn();
    ws.open_trust_leg(g, vec!["claude".to_string()]);
    assert_eq!(trust_to_read(&ws), None, "a row whose phase shows no screen is not read");
    let id = SupervisorIdentity { pid: 1, created: 100 };
    ws.begin_supervisor_epoch(id);
    crate::rows::run::observer::observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: id, run: None });
    assert_eq!(trust_to_read(&ws), Some((ws.workspace_id.clone(), g)));

    let root = Path::new("/unused");
    assert!(!trust_step(&workspaces, root, &ws.workspace_id, g + 1, Startup::MainBox, &Fake));
    assert!(trust_to_read(&ws).is_some(), "a main box read at another generation closes nothing");
    assert!(!trust_step(&workspaces, root, &ws.workspace_id, g, Startup::Neither, &Fake));
    assert!(trust_to_read(&ws).is_some(), "a screen that is neither closes nothing");
    assert!(!trust_step(&workspaces, root, &ws.workspace_id, g, Startup::MainBox, &Fake));
    assert_eq!(trust_to_read(&ws), None, "the main box closed the leg");
}

/// Effects that restart at once: the scope covers, nothing is recent, the stop succeeds and the spawn begins a generation
/// and opens a leg, as the real one does.
struct Fake;

impl TrustEffects for Fake {
    fn covered(&self, _: &Path) -> bool {
        true
    }
    fn recent_start(&self, _: &crate::rows::Workspace) -> bool {
        false
    }
    fn stop(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
    fn spawn(
        &self,
        _: &crate::rows::gate::StartPermit,
        _: &Path,
        _: &Path,
        argv: &[String],
        row: &crate::rows::Workspace,
        _: &Workspaces,
    ) -> std::io::Result<&'static str> {
        let generation = row.begin_spawn();
        row.open_trust_leg(generation, argv.to_vec());
        Ok("ready")
    }
}

/// Two Claude rows at the folder-trust dialog: one with an open leg is restarted and not stamped; one whose episode is
/// spent (its restart's leg shows the dialog again) is left to its person and stamped red. Fails if a dialog the restart
/// left is skipped by the stamp, or one it ran is stamped.
#[test]
fn a_trust_dialog_the_restart_leaves_is_still_stamped_red() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = json!({"agents": {"fresh": {"state": "working"}, "spent": {"state": "working"}}});
    std::fs::write(dir.path().join("registry.json"), registry.to_string()).expect("registry");
    let workspaces = Workspaces::new();
    let argv = vec!["claude".to_string()];
    let mut rows = Vec::new();
    let mut generations = Vec::new();
    for handle in ["fresh", "spent"] {
        let mut ws = crate::rows::Workspace::from_label(
            handle,
            PathBuf::from("/tmp").join(handle),
            false,
            "claude".to_string(),
            String::new(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        let ws = workspaces.insert(ws);
        let mut generation = ws.begin_spawn();
        ws.open_trust_leg(generation, argv.clone());
        if handle == "spent" {
            assert!(matches!(ws.take_trust_leg(generation, true), crate::rows::workspace::TrustTake::Leg(_)));
            generation = ws.begin_spawn();
            ws.open_trust_leg(generation, argv.clone());
        }
        generations.push((ws.clone(), generation));
        rows.push(Row { trust: Some((ws.workspace_id.clone(), generation)), ..plain(handle) });
    }
    let writes = std::cell::RefCell::new(Vec::new());
    let write = |h: &str, s: &Stamp| {
        writes.borrow_mut().push((h.to_string(), s.clone()));
        Ok(())
    };
    let trust = |id: &str, generation: u64, startup: Startup| trust_step(&workspaces, Path::new("/tmp/sot-trust-state"), id, generation, startup, &Fake);
    check_rows(dir.path(), &rows, &|_: &Path| Seen::Screen(trust_dialog()), &write, &trust, &mut Runs::default());
    assert_eq!(writes.into_inner(), vec![("spent".to_string(), Stamp::Dialog("claude dialog open".into()))]);
    assert_eq!(generations[0].0.spawn_generation(), generations[0].1 + 1, "the fresh row was restarted");
    assert_eq!(generations[1].0.spawn_generation(), generations[1].1, "the spent row was not");
}
