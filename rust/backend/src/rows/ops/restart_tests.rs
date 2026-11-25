use super::*;
use crate::rows::gate::StartPermit;
use crate::rows::workspace::{Observation, SupervisorIdentity};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

/// Effects that record their calls, and the argv a spawn was given.
#[derive(Default)]
struct Fake {
    stop_fails: bool,
    spawn_fails: bool,
    calls: RefCell<Vec<&'static str>>,
    spawned: RefCell<Vec<Vec<String>>>,
}

impl Fake {
    fn calls(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }
}

impl Respawn for Fake {
    fn stop(&self, _: &Path) -> Result<(), String> {
        self.calls.borrow_mut().push("stop");
        if self.stop_fails { Err("stop failed".into()) } else { Ok(()) }
    }
    fn spawn(&self, _: &StartPermit, _: &Path, _: &Path, argv: &[String], _: &Workspace, _: &Workspaces) -> std::io::Result<&'static str> {
        self.calls.borrow_mut().push("spawn");
        self.spawned.borrow_mut().push(argv.to_vec());
        if self.spawn_fails { Err(std::io::Error::other("spawn failed")) } else { Ok("ready") }
    }
}

fn recipe(_: &Path) -> Result<Vec<String>, String> {
    Ok(vec!["ccx".to_string(), "--capsule".to_string()])
}

/// A registered row of `agent`, Ready when `ready`.
fn registered(agent: &str, ready: bool) -> (Workspaces, Arc<Workspace>) {
    let workspaces = Workspaces::new();
    let mut row = Workspace::from_label("restart-me", PathBuf::from("/tmp/restart-me"), false, agent.to_string(), String::new(), String::new());
    row.runtime = "capsule".to_string();
    let ws = workspaces.insert(row);
    if ready {
        let observation = Observation::Phase { phase: Phase::Ready, supervisor: SupervisorIdentity { pid: 7, created: 7 }, run: None };
        assert!(ws.apply_phase_observation(observation));
    }
    (workspaces, ws)
}

fn quiet(_: &Workspace) -> Result<bool, String> {
    Ok(false)
}

fn unwoken(_: &Path) -> bool {
    true
}

/// `session` asked for by a caller outside any session, with the turn check and the mark's fact as given.
fn ask_with(
    workspaces: &Workspaces, session: &str, by_row: &str, turn: &dyn Fn(&Workspace) -> Result<bool, String>, not_woken: &dyn Fn(&Path) -> bool,
    recipe: &dyn Fn(&Path) -> Result<Vec<String>, String>, fx: &Fake,
) -> Result<WorkspaceRestartRes, Refused> {
    let world = World { turn_runs: turn, not_woken, recipe, fx };
    restart(workspaces, Path::new("/tmp/sot-restart-state"), &Asked { session, by_row, client: "client" }, &world)
}

fn ask(workspaces: &Workspaces, session: &str, turn: &dyn Fn(&Workspace) -> Result<bool, String>, fx: &Fake) -> Result<WorkspaceRestartRes, Refused> {
    ask_with(workspaces, session, "", turn, &unwoken, &recipe, fx)
}

fn refused_with(result: Result<WorkspaceRestartRes, Refused>) -> &'static str {
    result.expect_err("the restart was refused").code
}

/// Each refusal comes before any effect, in the plan's order, and a stop that fails starts nothing. A success stops, then
/// spawns with the recipe's argv. Fails if a check moves behind the stop, if the order is reversed or a refused row is
/// stopped.
#[test]
fn a_restart_checks_everything_before_it_stops_and_stops_before_it_spawns() {
    let fx = Fake::default();
    let never = |_: &Workspace| -> Result<bool, String> { panic!("the registry was asked before the kind and the phase were") };

    let (workspaces, ws) = registered("codex", true);
    for session in ["", "nobody", "ws-nobody-1"] {
        assert_eq!(refused_with(ask(&workspaces, session, &never, &fx)), "unknown_workspace", "{session:?}");
    }
    for agent in ["claude", "none"] {
        let (workspaces, ws) = registered(agent, true);
        let refusal = ask(&workspaces, &ws.slug, &never, &fx).expect_err("not Codex");
        assert_eq!(refusal.code, "not_codex");
        let kind = if agent == "none" { "no agent" } else { agent };
        assert_eq!(refusal.error, format!("restart-me runs {kind}, not Codex; only a Codex session's agent is started again this way."));
    }
    let (stopped, row) = registered("codex", false);
    let refusal = ask(&stopped, &row.slug, &never, &fx).expect_err("not ready");
    assert_eq!((refusal.code, refusal.error.starts_with("restart-me is not running (stopped). Open it")), ("not_ready", true));
    assert_eq!(refused_with(ask(&workspaces, &ws.slug, &|_| Ok(true), &fx)), "busy");

    let held = workspaces.capsule_guard(&ws.workspace_id).expect("guard");
    let held = held.try_lock().expect("held");
    assert_eq!(refused_with(ask(&workspaces, &ws.slug, &quiet, &fx)), "in_progress", "a held guard is never waited on");
    drop(held);
    let failing_recipe = |_: &Path| Err("no launcher".to_string());
    let refusal = ask_with(&workspaces, &ws.slug, "", &quiet, &unwoken, &failing_recipe, &fx).expect_err("no recipe");
    assert_eq!((refusal.code, refusal.error.as_str()), ("launcher_unresolved", "no launcher"));
    assert!(workspaces.close_gate_and_settle(std::time::Instant::now()));
    assert_eq!(refused_with(ask(&workspaces, &ws.slug, &quiet, &fx)), "closing");
    assert_eq!(fx.calls(), Vec::<&str>::new(), "a refusal made an effect");

    let (workspaces, ws) = registered("codex", true);
    let stuck = Fake { stop_fails: true, ..Fake::default() };
    let res = ask(&workspaces, &ws.workspace_id, &quiet, &stuck).expect("answered");
    assert_eq!((res.outcome.as_str(), res.detail.as_str(), res.slug.as_str()), ("stop_failed", "stop failed", "restart-me"));
    assert_eq!(stuck.calls(), ["stop"], "a leg that did not stop is not spawned over");

    let lost = Fake { spawn_fails: true, ..Fake::default() };
    let res = ask(&workspaces, &ws.slug, &quiet, &lost).expect("answered");
    assert_eq!((res.outcome.as_str(), res.detail.as_str()), ("not_started", "spawn failed"));
    assert_eq!(lost.calls(), ["stop", "spawn"]);

    let res = ask(&workspaces, &ws.slug, &quiet, &fx).expect("answered");
    assert_eq!((res.outcome.as_str(), res.workspace_id.as_str(), res.detail.as_str()), ("restarted", ws.workspace_id.as_str(), ""));
    assert_eq!(fx.calls(), ["stop", "spawn"]);
    assert_eq!(*fx.spawned.borrow(), vec![vec!["ccx".to_string(), "--capsule".to_string()]]);
}

/// A session that is not marked not woken by mail is left alone (`not_needed`), and so is the asker's own row by whatever name it is
/// asked for (`own_session`, an id or a slug), each before the registry is read and before any effect; a session whose
/// turn cannot be told because the registry cannot be read is refused too (`registry_unreadable`), never read as at
/// rest. Red if any of the three is dropped: the restart then stops the agent.
#[test]
fn a_restart_leaves_a_woken_session_the_askers_own_and_one_with_an_unreadable_registry_alone() {
    let fx = Fake::default();
    let never = |_: &Workspace| -> Result<bool, String> { panic!("the registry was asked first") };
    let (workspaces, ws) = registered("codex", true);
    let woken = |_: &Path| false;
    let refusal = ask_with(&workspaces, &ws.slug, "", &never, &woken, &recipe, &fx).expect_err("not marked");
    assert_eq!(refusal.code, "not_needed");
    assert!(refusal.error.contains("so a restart is not needed and would end its conversation."), "{}", refusal.error);
    for named in [ws.slug.as_str(), ws.workspace_id.as_str()] {
        let refusal = ask_with(&workspaces, named, &ws.workspace_id, &never, &unwoken, &recipe, &fx).expect_err("its own");
        assert_eq!((refusal.code, refusal.error.as_str()), ("own_session", "a session cannot restart its own agent. Ask its person to run this in another session or a shell on this computer."));
    }
    let (other, _) = registered("codex", true);
    assert_eq!(refused_with(ask_with(&workspaces, &ws.slug, &other.list()[0].workspace_id, &|_| Ok(true), &unwoken, &recipe, &fx)), "busy", "another row's id is not the row's own");
    let unreadable = |_: &Workspace| -> Result<bool, String> { Err("the comm registry is not valid JSON: x".to_string()) };
    let refusal = ask(&workspaces, &ws.slug, &unreadable, &fx).expect_err("unreadable");
    assert_eq!(refusal.code, "registry_unreadable");
    assert!(refusal.error.contains("the comm registry is not valid JSON: x"), "{}", refusal.error);
    assert_eq!(fx.calls(), Vec::<&str>::new(), "a refusal made an effect");
}
