use super::*;

#[test]
fn a_restart_spawns_the_rows_recipe_never_a_reauths_resume() {
    let reauth: Vec<String> = ["claude", "--resume", "abc", "/sot-session-start"].map(String::from).to_vec();
    let recipe: Vec<String> = ["claude", "/sot-session-start"].map(String::from).to_vec();
    assert_eq!(restart_argv(Ok(recipe.clone()), &reauth), recipe);
    assert_eq!(restart_argv(Err("no claude".into()), &reauth), recipe);
}

#[test]
fn exit_codes_classify_as_the_supervisor_returns_them() {
    assert!(matches!(classify_exit_code(Some(0)), LegOutcome::Clean));
    assert!(matches!(classify_exit_code(Some(69)), LegOutcome::Terminal));
    assert!(matches!(classify_exit_code(Some(70)), LegOutcome::Contended));
    assert!(matches!(classify_exit_code(Some(76)), LegOutcome::AgentEnded));
    // 75, the agent could not be spawned, is a crash: restarted with the same argv.
    for code in [Some(1), Some(2), Some(-1), Some(75), None] {
        assert!(matches!(classify_exit_code(code), LegOutcome::Crash));
    }
}

#[test]
fn restart_budget_numbers_match_adr_0041s_own_launcher_table() {
    assert_eq!(RESTART_BACKOFFS.len(), 5);
    assert_eq!(MAX_RESTARTS_PER_WINDOW, 5);
    assert_eq!(RESTART_WINDOW, std::time::Duration::from_secs(60));
    assert_eq!(
        RESTART_BACKOFFS.map(|d| d.as_secs()),
        [1, 3, 7, 15, 30]
    );
}

#[test]
fn supervisors_that_never_were_told_ready_are_a_count_not_a_rate() {
    let mut budget = Budget::default();
    let t0 = Instant::now();
    // Hours between exits change nothing: five restarts, backing off 1, 3, 7, 15 and 30 s, then the give-up.
    for (i, want) in RESTART_BACKOFFS.iter().enumerate() {
        let now = t0 + std::time::Duration::from_secs(10_000 * (i as u64 + 1));
        assert_eq!(budget.on_exit(false, now), Some(*want));
        budget.restarted(now);
    }
    assert_eq!(budget.on_exit(false, t0 + std::time::Duration::from_secs(99_999)), None);
}

#[test]
fn a_supervisor_that_was_told_ready_resets_the_count_and_keeps_the_window() {
    let mut budget = Budget::default();
    let t0 = Instant::now();
    for _ in 0..4 {
        assert!(budget.on_exit(false, t0).is_some());
        budget.restarted(t0);
    }
    // One that was observed ready: the count starts over, and its own restarts are judged by the window.
    assert_eq!(budget.on_exit(true, t0), Some(RESTART_BACKOFFS[0]));
    budget.restarted(t0);
    for _ in 0..5 {
        assert!(budget.on_exit(false, t0).is_some());
        budget.restarted(t0);
    }
    assert!(budget.on_exit(false, t0).is_none(), "five never-ready restarts since the ready one");
    let mut budget = Budget::default();
    for i in 0..MAX_RESTARTS_PER_WINDOW {
        let at = t0 + std::time::Duration::from_secs(i as u64);
        assert!(budget.on_exit(true, at).is_some());
        budget.restarted(at);
    }
    assert!(budget.on_exit(true, t0 + std::time::Duration::from_secs(10)).is_none(), "five ready restarts in the window");
    assert!(
        budget.on_exit(true, t0 + RESTART_WINDOW + std::time::Duration::from_secs(10)).is_some(),
        "the window slid past them"
    );
}

#[test]
fn a_terminal_mark_admits_only_a_row_nothing_answers() {
    use crate::rows::workspace::Phase;
    // `stopped` is what a Windows first start reads before its supervisor takes the fence.
    for phase in [Phase::Unreachable, Phase::Stopped] {
        assert!(nothing_answers(phase.as_wire_str()), "{phase:?} is a row nothing answers");
    }
    for phase in [Phase::Starting, Phase::Ready, Phase::Ending, Phase::EndedNoRespawn, Phase::Terminal, Phase::Foreign] {
        assert!(!nothing_answers(phase.as_wire_str()), "{phase:?} is a row something answers");
    }
}

/// The watchdog an adopted exit installs owns the row before `install_watchdog` returns, under its caller's guard: the
/// task that restarts it has not run yet (a current-thread runtime), and its recheck would decline (no fence file), so
/// ownership announced inside the task would leave the row ownerless for a moment.
#[tokio::test]
async fn an_adopted_exit_owns_the_row_before_install_returns() {
    let state = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new();
    let mut row = crate::rows::Workspace::from_label(
        "adopted-exit",
        PathBuf::from("/tmp/sot-adopted-exit"),
        false,
        "none".to_string(),
        String::new(),
        String::new(),
    );
    row.runtime = "capsule".to_string();
    let ws = workspaces.insert(row);
    assert_eq!(ws.watchdog_owner(), None);
    install_watchdog(
        ws.workspace_id.clone(),
        PathBuf::from("sot-capsule"),
        state.path().to_path_buf(),
        Vec::new(),
        PathBuf::from("/tmp"),
        String::new(),
        ws.slug.clone(),
        First::Exited(None, ws.spawn_generation()),
        workspaces.clone(),
    );
    assert!(ws.watchdog_owner().is_some(), "the adopted exit's watchdog owns the row when install returns");
}

fn registered_row(label: &str) -> (Workspaces, std::sync::Arc<crate::rows::Workspace>) {
    let workspaces = Workspaces::new();
    let mut row = crate::rows::Workspace::from_label(
        label,
        PathBuf::from(format!("/tmp/sot-{label}")),
        false,
        "none".to_string(),
        String::new(),
        String::new(),
    );
    row.runtime = "capsule".to_string();
    let ws = workspaces.insert(row);
    (workspaces, ws)
}

fn watch_exit(ws: &crate::rows::Workspace, workspaces: &Workspaces, state: &Path, code: Option<i32>, generation: u64) {
    install_watchdog(
        ws.workspace_id.clone(),
        PathBuf::from("sot-capsule"),
        state.to_path_buf(),
        Vec::new(),
        PathBuf::from("/tmp"),
        String::new(),
        ws.slug.clone(),
        First::Exited(code, generation),
        workspaces.clone(),
    );
}

/// A watchdog's outcome names the spawn it came from. S1 exits 69 and S1's watchdog has not yet handled it when a
/// replacement S2 is launched and watched: the exit must not latch S2 terminal. The control is the same exit handled at
/// its own generation, with S1 the supervisor the record names, which does latch.
#[tokio::test]
async fn an_exit_69_of_a_superseded_supervisor_latches_nothing_of_its_replacement() {
    use crate::rows::workspace::{Observation, Phase, SupervisorIdentity};
    let state = tempfile::tempdir().unwrap();
    let id = |pid| SupervisorIdentity { pid, created: 100 };

    let (workspaces, ws) = registered_row("superseded-69");
    let g1 = ws.begin_spawn();
    assert!(ws.record_watch(g1, id(1), true));
    ws.begin_supervisor_epoch(id(1));
    // S1's exit is in flight; reauth launches S2 and the observer records it.
    let g2 = ws.begin_spawn();
    assert!(ws.record_watch(g2, id(2), true));
    ws.begin_supervisor_epoch(id(2));
    observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: id(2), run: None });
    watch_exit(&ws, &workspaces, state.path(), Some(69), g1);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(ws.phase(), Phase::Ready, "S1's exit 69 latched S2 terminal");
    assert_eq!(ws.watched().map(|w| w.supervisor), Some(id(2)));

    let (workspaces, ws) = registered_row("own-69");
    let g = ws.begin_spawn();
    assert!(ws.record_watch(g, id(1), true));
    ws.begin_supervisor_epoch(id(1));
    observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: id(1), run: None });
    watch_exit(&ws, &workspaces, state.path(), Some(69), g);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(ws.phase(), Phase::Terminal, "a supervisor's own exit 69 marks it terminal");
}

/// A 69 no watch ever named (the record is empty) is latched under the guard once nothing answers, at its own generation
/// only.
#[tokio::test]
async fn an_exit_69_with_no_record_latches_a_row_nothing_answers_at_its_own_generation_only() {
    use crate::rows::workspace::Phase;
    let state = tempfile::tempdir().unwrap();
    let (workspaces, ws) = registered_row("unnamed-69");
    let g1 = ws.begin_spawn();
    let _g2 = ws.begin_spawn();
    watch_exit(&ws, &workspaces, state.path(), Some(69), g1);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_ne!(ws.phase(), Phase::Terminal, "a superseded spawn's 69 latched the row");

    let (workspaces, ws) = registered_row("unnamed-69-own");
    let g = ws.begin_spawn();
    watch_exit(&ws, &workspaces, state.path(), Some(69), g);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(ws.phase(), Phase::Terminal, "a 69 no watch named, with nothing answering, latches the row");
}
