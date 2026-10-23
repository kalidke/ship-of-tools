use super::*;

#[test]
fn workspace_from_label_uses_slug_and_tmux_convention() {
    let ws = Workspace::from_label("MyPkg.jl", PathBuf::from("/home/u/MyPkg.jl"), false, "none".into(), String::new(), String::new());
    assert_eq!(ws.slug, "mypkg_jl");
    assert_eq!(ws.session_name, "sot-be-mypkg_jl");
    assert_eq!(ws.label, "MyPkg.jl");
    assert_eq!(ws.project_root, PathBuf::from("/home/u/MyPkg.jl"));
}

#[test]
fn workspace_kernel_built_reflects_lazy_construction() {
    // Fresh workspace: handle not yet constructed.
    let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    assert!(!ws.kernel_built());
    // Calling .kernel() constructs the handle (no child spawned yet).
    let _ = ws.kernel();
    assert!(ws.kernel_built());
}

#[test]
fn every_phase_has_a_distinct_wire_string() {
    let all = [
        Phase::Stopped,
        Phase::Starting,
        Phase::Ready,
        Phase::Ending,
        Phase::EndedNoRespawn,
        Phase::Terminal,
        Phase::Unreachable,
        Phase::Foreign,
    ];
    let strings: std::collections::HashSet<&str> = all.iter().map(|p| p.as_wire_str()).collect();
    assert_eq!(strings.len(), all.len(), "every Phase must have its own distinct wire string");
}

fn observed(phase: Phase, supervisor: SupervisorIdentity, run: Option<uuid::Uuid>) -> Observation {
    Observation::Phase { phase, supervisor, run }
}

/// The cell orders on the run, not on a voyage: a supervisor that rested on run R1 latches `ended_no_respawn`, a reset's
/// run R2 releases the latch, and a stale resting observation of R1 that arrives after R2 is lower and is rejected.
#[test]
fn the_cell_on_the_run_never_latches_a_resting_row_after_a_reset() {
    let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    let supervisor = SupervisorIdentity { pid: 7, created: 70 };
    let (r1, r2) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    assert!(r1 < r2, "runs of one supervisor order by time");
    ws.begin_supervisor_epoch(supervisor);

    assert!(ws.apply_phase_observation(observed(Phase::Ready, supervisor, Some(r1))));
    assert!(ws.apply_phase_observation(observed(Phase::EndedNoRespawn, supervisor, Some(r1))));
    assert_eq!(ws.phase(), Phase::EndedNoRespawn);
    // The latch holds against the run it rests on.
    assert!(!ws.apply_phase_observation(observed(Phase::Ready, supervisor, Some(r1))));
    assert_eq!(ws.phase(), Phase::EndedNoRespawn);

    // A reset starts R2: its first observation releases the latch.
    assert!(ws.apply_phase_observation(observed(Phase::Ready, supervisor, Some(r2))));
    assert_eq!(ws.phase(), Phase::Ready);

    // A stale resting observation of R1, read before the reset and delivered after it, is lower and is rejected.
    assert!(!ws.apply_phase_observation(observed(Phase::EndedNoRespawn, supervisor, Some(r1))));
    assert_eq!(ws.phase(), Phase::Ready);
    // So is a resting observation that names no run at all, once the cell holds one.
    assert!(!ws.apply_phase_observation(observed(Phase::EndedNoRespawn, supervisor, None)));
    assert_eq!(ws.phase(), Phase::Ready);
}

/// A supervisor that rests before any run (a `--resume` over a requested end) names no run: its latch is released by the
/// first run of the reset that follows.
#[test]
fn a_latch_on_no_run_is_released_by_the_first_run() {
    let ws = Workspace::from_label("beta", PathBuf::from("/p/beta"), false, "none".into(), String::new(), String::new());
    let supervisor = SupervisorIdentity { pid: 8, created: 80 };
    ws.begin_supervisor_epoch(supervisor);
    assert!(ws.apply_phase_observation(observed(Phase::EndedNoRespawn, supervisor, None)));
    assert!(!ws.apply_phase_observation(observed(Phase::Ready, supervisor, None)));
    assert!(ws.apply_phase_observation(observed(Phase::Ready, supervisor, Some(uuid::Uuid::now_v7()))));
    assert_eq!(ws.phase(), Phase::Ready);
}
