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

/// The settle ends at the first answer of a supervisor, `starting` included: it is a bound on reachability under the
/// guard, and a start reads `starting` until the agent has lived 2 s. Only an unanswering row keeps it waiting.
#[test]
fn a_settle_ends_at_the_first_answer() {
    use crate::rows::workspace::Phase;
    for phase in [Phase::Starting, Phase::Ready, Phase::Ending, Phase::EndedNoRespawn, Phase::Terminal, Phase::Foreign, Phase::Stopped] {
        assert!(settled(phase.as_wire_str()), "{phase:?} is an answer");
    }
    assert!(!settled(Phase::Unreachable.as_wire_str()), "nothing answered yet");
}

/// Pins `open_leg_after_start` and a failed spawn: the helper opens a leg only for a Claude row, with the argv it is
/// handed (a reauth's `--resume <id>` included), and the next spawn closes it; a failed spawn opens none. That the
/// spawners' `Started` arms call the helper is read, not tested: a unit test cannot start a supervisor.
#[tokio::test]
async fn only_a_started_claude_spawn_opens_a_leg_and_the_next_spawn_closes_it() {
    use crate::rows::workspace::TrustTake;
    let tmp = tempfile::tempdir().unwrap();
    // `spawn_and_watch` resolves the state root from the process environment: pin it to this test's own folder, under the
    // env lock, so another test's pin (and its removal) cannot move it mid-body.
    let _env = crate::paths::EnvPinned::new(tmp.path());
    let workspaces = crate::rows::Workspaces::new();
    let permit = workspaces.begin_start("any").unwrap();
    let row = |label: &str, kind: &str| {
        let root = tmp.path().join(label);
        std::fs::create_dir_all(&root).unwrap();
        let mut row = crate::rows::Workspace::from_label(
            label, root, false, kind.to_string(), String::new(), String::new(),
        );
        row.runtime = "capsule".to_string();
        workspaces.insert(row)
    };
    let reauth = vec!["claude".to_string(), "--resume".to_string(), "sid-1".to_string()];

    let codex = row("codex", "codex");
    let g = codex.begin_spawn();
    open_leg_after_start(&workspaces, &codex.workspace_id, g, "codex", vec!["codex".to_string()]);
    assert_eq!(codex.trust_reading(), None, "a Codex row opens no leg");

    // A failed spawn: the state folder lies inside the project root, so it returns before anything started. The folder
    // exists, so both sides canonicalize alike (a missing one keeps its raw spelling, which on Windows differs from the
    // root's `\\?\` form and escapes the containment check).
    let ws = row("claude", "claude");
    let state = ws.project_root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let failed = spawn_and_watch(
        &permit, Path::new("sot-capsule"), &state, StartMode::Start, &reauth, &ws.project_root, "",
        ws.workspace_id.clone(), ws.slug.clone(), workspaces.clone(),
    );
    let refusal = failed.expect_err("a state folder inside the project root is refused");
    assert!(refusal.to_string().contains("lies inside the project root"), "refused for that reason: {refusal}");
    assert_eq!(ws.trust_reading(), None, "a failed spawn opens no leg");

    let g = ws.begin_spawn();
    open_leg_after_start(&workspaces, &ws.workspace_id, g, "claude", reauth.clone());
    assert_eq!(ws.trust_reading(), Some(g));
    assert_eq!(ws.take_trust_leg(g, true), TrustTake::Leg(reauth.clone()), "the reauth's argv is the one recorded");

    let other = row("other", "claude");
    let g = other.begin_spawn();
    open_leg_after_start(&workspaces, &other.workspace_id, g, "claude", reauth.clone());
    other.begin_spawn();
    assert_eq!(other.trust_reading(), None, "the next spawn closed the leg");
}
