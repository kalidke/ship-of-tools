use super::*;
use crate::rows::spawn::state_root::state_dir_for;

// #8: every run start passes the gate. The compiler enumerates spawns
// through `spawn_detached_supervisor`'s permit parameter; this pins
// what it cannot see: that a closing gate refuses a start and a reset
// before anything is located, claimed or dialled.
#[test]
fn closed_gate_refuses_start_and_reset() {
    let reg = crate::rows::Workspaces::new();
    assert!(reg.close_gate_and_settle(std::time::Instant::now()));
    let root = tempfile::tempdir().unwrap();
    let refusal = "workspace ws-gate-1 cannot start: this computer's backend is shutting down".to_string();
    let started = start_supervisor(
        root.path(), "ws-gate-1", StartMode::Start, &["true".to_string()], root.path(), "", "gate", reg.clone(),
    );
    assert_eq!(started, Err(refusal.clone()), "start_supervisor with the gate closing");
    let reset = reset_run(&reg, "ws-gate-1", &state_dir_for(root.path(), "ws-gate-1"));
    assert_eq!(reset, Err(refusal), "reset_run with the gate closing");
    assert!(!state_dir_for(root.path(), "ws-gate-1").exists(), "a refused start claimed nothing: no state folder was made");
}
