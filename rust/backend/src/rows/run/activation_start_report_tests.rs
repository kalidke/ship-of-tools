use super::*;
use crate::rows::workspace::{Observation, Phase, SupervisorIdentity};
use crate::rows::Workspace;

/// A row latched terminal (its supervisor exited 69) is not started by an attach: the resume returns the latch and
/// spawns nothing, so the attach reports nothing started and `pty.open` logs no "capsule supervisor started on
/// attach". Fails while every resume counts as a start.
#[test]
fn an_attach_to_a_terminal_row_reports_nothing_started() {
    let state_root = tempfile::tempdir().unwrap();
    let reg = Workspaces::new();
    let mut ws = Workspace::from_label("t", std::path::PathBuf::from("/p/t"), false, "none".into(), String::new(), String::new());
    ws.runtime = "capsule".to_string();
    let (id, slug) = (ws.workspace_id.clone(), ws.slug.clone());
    reg.insert(ws);
    let row = reg.resolve(Some(id.as_str())).expect("the row");
    let identity = SupervisorIdentity { pid: 1, created: 1 };
    row.begin_supervisor_epoch(identity);
    row.apply_phase_observation(Observation::Phase { phase: Phase::Terminal, supervisor: identity, voyage: None });
    // A published pointer with nothing answering its lane: the lane reads unreachable, as after the exit 69.
    let state_dir = super::super::state_dir_for(state_root.path(), &id);
    std::fs::create_dir_all(&state_dir).unwrap();
    sot_log::supervisor::journal::pointer::publish(&state_dir, "a1b2c3d4-e5f6-4890-9abc-def012345678").unwrap();
    let started = ensure_started(state_root.path(), &id, "none", "", &slug, Path::new("/p/t"), ActivationIntent::Selection, reg.clone());
    assert_eq!(started, Ok(None), "a terminal row was reported started");
    assert_eq!(row.phase(), Phase::Terminal);
}

/// A row that never rests within the settle budget (here its watchdog owns an unreachable lane, as in a restart
/// backoff longer than the budget) is a failed activation, as at the pending bound: the caller gets the reason as an
/// error and the row's `activation_error` holds it. Fails while an exhausted wait returns as if it had resolved.
#[test]
fn a_wait_that_outlasts_the_settle_budget_fails_the_activation() {
    let state_root = tempfile::tempdir().unwrap();
    let reg = Workspaces::new();
    let mut ws = Workspace::from_label("u", std::path::PathBuf::from("/p/u"), false, "none".into(), String::new(), String::new());
    ws.runtime = "capsule".to_string();
    let (id, slug) = (ws.workspace_id.clone(), ws.slug.clone());
    reg.insert(ws);
    let row = reg.resolve(Some(id.as_str())).expect("the row");
    row.set_watchdog_owner(1);
    let state_dir = super::super::state_dir_for(state_root.path(), &id);
    std::fs::create_dir_all(&state_dir).unwrap();
    sot_log::supervisor::journal::pointer::publish(&state_dir, "a1b2c3d4-e5f6-4890-9abc-def012345678").unwrap();
    let result = ensure_started(state_root.path(), &id, "none", "", &slug, Path::new("/p/u"), ActivationIntent::Selection, reg.clone());
    assert!(result.is_err(), "an exhausted settle wait was reported as resolved: {result:?}");
    assert_eq!(row.activation_error(), result.err(), "the row must carry the reason");
}
