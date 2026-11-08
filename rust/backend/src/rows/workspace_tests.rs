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
fn repeated_and_concurrent_mints_of_one_slug_have_distinct_ids() {
    let ids = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8).map(|_| scope.spawn(|| {
            (0..64).map(|_| {
                let row = Workspace::from_label("same-label", PathBuf::from("/project"), false,
                    "none".into(), String::new(), String::new());
                (row.workspace_id, row.created)
            }).collect::<Vec<_>>()
        })).collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect::<Vec<_>>()
    });
    let seconds: std::collections::HashSet<_> = ids.iter().map(|(_, created)| created).collect();
    assert!(seconds.len() < ids.len(), "setup: at least two mints occurred within one second");
    let unique: std::collections::HashSet<_> = ids.iter().map(|(id, _)| id).collect();
    assert_eq!(unique.len(), ids.len(), "each mint must have its own identity");
    for (id, _) in ids {
        assert!(crate::rows::state_folder::is_row_folder_name(&id), "{id}");
        let suffix = id.strip_prefix("ws-same-label-").expect("the id keeps its slug prefix");
        assert!(!suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
        let leaf = crate::comm::identity::self_file::slot_leaf("box", &id).unwrap();
        assert_eq!(crate::comm::identity::self_file::row_of(std::path::Path::new(&leaf), "box"), Some(id));
    }
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

#[test]
fn a_taken_trust_leg_is_spent_until_the_main_box_and_other_answers_close_it() {
    let row = || Workspace::from_label("t", PathBuf::from("/p/t"), false, "claude".into(), String::new(), String::new());
    let argv = vec!["claude".to_string(), "--resume".to_string(), "abc".to_string()];
    let ws = row();
    let g = ws.begin_spawn();
    assert_eq!(ws.trust_reading(), None);
    assert_eq!(ws.take_trust_leg(g, true), TrustTake::NotOpen, "no leg opened");
    ws.open_trust_leg(g, argv.clone());
    assert_eq!(ws.trust_reading(), Some(g));
    assert_eq!(ws.take_trust_leg(g, true), TrustTake::Leg(argv.clone()));
    assert_eq!(ws.trust_reading(), Some(g), "spent rows are still read");
    // The restart's own spawn opens a new leg; the episode is spent, so it is left to its person.
    let g2 = ws.begin_spawn();
    ws.open_trust_leg(g2, argv.clone());
    assert_eq!(ws.take_trust_leg(g2, true), TrustTake::Spent);
    assert_eq!(ws.take_trust_leg(g2, true), TrustTake::Spent, "still spent, with no leg to close");
    ws.trust_passed(g2);
    assert_eq!(ws.trust_reading(), None);
    ws.open_trust_leg(g2, argv.clone());
    assert_eq!(ws.take_trust_leg(g2, true), TrustTake::Leg(argv.clone()), "a new episode can restart again");
    // A moved generation is not open; a stale open is refused.
    let ws = row();
    let g = ws.begin_spawn();
    ws.open_trust_leg(g, argv.clone());
    let g2 = ws.begin_spawn();
    assert_eq!(ws.take_trust_leg(g, true), TrustTake::NotOpen);
    ws.open_trust_leg(g, argv.clone());
    assert_eq!(ws.take_trust_leg(g2, true), TrustTake::NotOpen, "a spawn closed the leg and a stale open is refused");
    ws.open_trust_leg(g2, argv.clone());
    assert_eq!(ws.take_trust_leg(g, true), TrustTake::NotOpen, "a stale generation takes nothing");
    assert_eq!(ws.trust_reading(), Some(g2), "and leaves the open leg");
    ws.trust_passed(g);
    assert_eq!(ws.trust_reading(), Some(g2), "a main box read at an earlier generation clears nothing of this leg");
    // An uncovered root closes the leg and spends nothing.
    ws.open_trust_leg(g2, argv.clone());
    assert_eq!(ws.take_trust_leg(g2, false), TrustTake::Outside);
    assert_eq!(ws.trust_reading(), None);
    assert_eq!(ws.take_trust_leg(g2, true), TrustTake::NotOpen);
}
