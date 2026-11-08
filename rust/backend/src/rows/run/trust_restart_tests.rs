use super::*;
use crate::rows::run::start::open_leg_after_start;
use crate::rows::workspace::TrustTake;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

/// Effects that record their calls. Its spawn begins a generation and opens a leg, as the real one does.
struct Fake {
    covered: bool,
    recent: RefCell<bool>,
    stop_fails: bool,
    calls: RefCell<Vec<&'static str>>,
    spawned: RefCell<Vec<Vec<String>>>,
}

impl Fake {
    fn new(covered: bool) -> Self {
        Fake { covered, recent: RefCell::new(false), stop_fails: false, calls: RefCell::default(), spawned: RefCell::default() }
    }
    fn calls(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }
}

impl TrustEffects for Fake {
    fn covered(&self, _: &Path) -> bool {
        self.calls.borrow_mut().push("covered");
        self.covered
    }
    fn recent_start(&self, _: &Workspace) -> bool {
        *self.recent.borrow()
    }
    fn stop(&self, _: &Path) -> Result<(), String> {
        self.calls.borrow_mut().push("stop");
        if self.stop_fails {
            Err("stop failed".into())
        } else {
            Ok(())
        }
    }
    fn spawn(&self, _: &StartPermit, _: &Path, _: &Path, argv: &[String], row: &Workspace, workspaces: &Workspaces) -> std::io::Result<&'static str> {
        self.calls.borrow_mut().push("spawn");
        self.spawned.borrow_mut().push(argv.to_vec());
        let generation = row.begin_spawn();
        open_leg_after_start(workspaces, &row.workspace_id, generation, "claude", argv.to_vec());
        Ok("ready")
    }
}

fn argv() -> Vec<String> {
    ["claude", "--resume", "0f0e0d0c-aaaa-4bbb-8ccc-000000000001"].map(String::from).to_vec()
}

/// A registered Claude row whose leg is open at its current generation.
fn leg_row(label: &str) -> (Workspaces, Arc<Workspace>, u64) {
    let workspaces = Workspaces::new();
    let mut row = Workspace::from_label(label, PathBuf::from("/tmp").join(label), false, "claude".to_string(), String::new(), String::new());
    row.runtime = "capsule".to_string();
    let ws = workspaces.insert(row);
    let generation = ws.begin_spawn();
    ws.open_trust_leg(generation, argv());
    (workspaces, ws, generation)
}

fn restart(workspaces: &Workspaces, ws: &Workspace, generation: u64, fx: &Fake) -> TrustRestart {
    restart_at_trust_dialog(workspaces, Path::new("/tmp/sot-trust-state"), &ws.workspace_id, generation, fx)
}

/// A leg at the dialog is stopped, then spawned with its own argv (a reauth's `--resume <id>` kept); the spawn is a new
/// generation whose leg is open and whose episode is spent. Fails if the order is reversed, the argv is rebuilt or a
/// stop is skipped.
#[test]
fn a_leg_at_the_trust_dialog_is_stopped_then_spawned_with_its_own_argv() {
    let (workspaces, ws, g) = leg_row("restart-order");
    let fx = Fake::new(true);
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Restarted);
    assert_eq!(fx.calls(), ["covered", "stop", "spawn"]);
    assert_eq!(*fx.spawned.borrow(), vec![argv()]);
    assert_eq!(ws.spawn_generation(), g + 1);
    assert_eq!(ws.trust_reading(), Some(g + 1), "the new leg is watched for its main box");

    let (workspaces, ws, g) = leg_row("restart-stop-fails");
    let fx = Fake { stop_fails: true, ..Fake::new(true) };
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Failed);
    assert_eq!(fx.calls(), ["covered", "stop"], "a leg that did not stop is not spawned over");
}

/// A root outside the prefix is never restarted, and its leg is closed; one inside is.
#[test]
fn a_root_outside_the_prefix_is_never_restarted_while_one_inside_is() {
    let (workspaces, ws, g) = leg_row("outside");
    let fx = Fake::new(false);
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Left);
    assert_eq!(fx.calls(), ["covered"]);
    assert_eq!(ws.trust_reading(), None, "the outside leg is closed");

    let (workspaces, ws, g) = leg_row("inside");
    let fx = Fake::new(true);
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Restarted);
}

/// A second dialog before the main box is left to its person; once a main box has shown, a later leg restarts again.
#[test]
fn a_second_trust_dialog_before_the_main_box_is_left_to_its_person() {
    let (workspaces, ws, g) = leg_row("second");
    let fx = Fake::new(true);
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Restarted);
    let g2 = ws.spawn_generation();
    assert_eq!(restart(&workspaces, &ws, g2, &fx), TrustRestart::Left, "the restart's own leg shows the dialog again");
    assert_eq!(fx.spawned.borrow().len(), 1, "one restart");
    assert_eq!(restart(&workspaces, &ws, g2, &fx), TrustRestart::Left);

    ws.trust_passed(g2);
    ws.open_trust_leg(g2, argv());
    assert_eq!(restart(&workspaces, &ws, g2, &fx), TrustRestart::Restarted, "a new episode restarts again");
    assert_eq!(fx.spawned.borrow().len(), 2);
}

/// Nothing is stopped or spawned, and nothing is spent, by a busy guard, a moved generation, a row gone or a closing
/// gate; the busy guard is tried again, and an open leg survives each.
#[test]
fn a_busy_guard_a_moved_generation_a_gone_row_or_a_closing_gate_restarts_nothing() {
    let (workspaces, ws, g) = leg_row("refused");
    let fx = Fake::new(true);
    let guard = workspaces.capsule_guard(&ws.workspace_id).expect("guard");
    let held = guard.try_lock().expect("held");
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Waiting, "a busy guard is never waited on");
    drop(held);
    assert_eq!(fx.calls(), Vec::<&str>::new());
    assert_eq!(ws.trust_reading(), Some(g), "the leg stays open for the next tick");

    assert_eq!(restart(&workspaces, &ws, g + 1, &fx), TrustRestart::Left, "a generation that moved");
    assert_eq!(restart_at_trust_dialog(&workspaces, Path::new("/tmp/x"), "ws-nowhere-1", 1, &fx), TrustRestart::Left, "a row gone");
    assert!(workspaces.close_gate_and_settle(std::time::Instant::now()));
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Left, "a closing gate");
    assert_eq!(fx.calls(), ["covered"], "only the scope was asked, and nothing moved");
    assert_eq!(ws.take_trust_leg(g, true), TrustTake::Leg(argv()), "a refusal before the take leaves the leg open and the episode unspent");
}

/// A restart waits out a Claude start on the same trust file less than two seconds old, taking nothing; it runs once
/// the start is older.
#[test]
fn a_restart_waits_out_a_recent_claude_start_on_the_same_trust_file() {
    let (workspaces, ws, g) = leg_row("recent");
    let fx = Fake::new(true);
    *fx.recent.borrow_mut() = true;
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Waiting);
    assert_eq!(fx.calls(), ["covered"]);
    assert_eq!(ws.trust_reading(), Some(g), "the decline closed nothing and spent nothing");
    *fx.recent.borrow_mut() = false;
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Restarted);
}

/// The real effect asks about the file a preparation marks: the same home, account and environment. A different
/// spelling would make the wait never wait.
#[test]
fn the_wait_asks_about_the_file_a_real_preparation_marks() {
    if !sot_platform::test_isolated::run_isolated("rows::run::trust_restart::tests::the_wait_asks_about_the_file_a_real_preparation_marks") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let home = sot_platform::host::paths::simplify_verbatim(temp.path().canonicalize().unwrap());
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
    std::env::set_var("LOCALAPPDATA", home.join("local"));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    std::fs::create_dir_all(home.join(".claude-auth/dst")).unwrap();
    let cwd = home.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let (workspaces, ws, _) = leg_row("real-key");
    assert!(!LiveTrust.recent_start(&ws), "nothing prepared yet");
    crate::agents::env::account_spawn_env("claude", "", &cwd, &ws.workspace_id).unwrap();
    workspaces.set_account(&ws.workspace_id, "dst").expect("the row");
    assert!(!LiveTrust.recent_start(&ws), "the default account's preparation does not mark the row's own account");
    crate::agents::env::account_spawn_env("claude", "dst", &cwd, &ws.workspace_id).unwrap();
    assert!(LiveTrust.recent_start(&ws), "the preparation of the row's own account marks the file the restart asks about");
}

/// A closed leg is not looked up at all, and a root outside the prefix is closed at once rather than waiting out a recent
/// start on the trust file.
#[test]
fn a_closed_leg_is_not_looked_up_and_an_outside_root_does_not_wait() {
    let (workspaces, ws, g) = leg_row("closed-leg");
    let fx = Fake::new(true);
    ws.trust_passed(g);
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Left);
    assert_eq!(fx.calls(), Vec::<&str>::new(), "a closed leg is not looked up");

    let (workspaces, ws, g) = leg_row("outside-recent");
    let fx = Fake::new(false);
    *fx.recent.borrow_mut() = true;
    assert_eq!(restart(&workspaces, &ws, g, &fx), TrustRestart::Left, "an outside root is not made to wait");
    assert_eq!(ws.trust_reading(), None, "and its leg is closed");
}
