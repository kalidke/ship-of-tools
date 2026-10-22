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
fn supervisors_that_never_reached_ready_are_a_count_not_a_rate() {
    let mut budget = Budget::default();
    let t0 = Instant::now();
    // Hours between exits change nothing: five restarts, backing off 1, 3, 7, 15 and 30 s, then the give-up.
    for (i, want) in RESTART_BACKOFFS.iter().enumerate() {
        let now = t0 + std::time::Duration::from_secs(10_000 * (i as u64 + 1));
        assert_eq!(budget.on_exit(std::time::Duration::from_millis(100), now), Some(*want));
        budget.restarted(now);
    }
    assert_eq!(budget.on_exit(std::time::Duration::from_millis(100), t0 + std::time::Duration::from_secs(99_999)), None);
}

#[test]
fn a_supervisor_that_reached_ready_resets_the_count_and_keeps_the_window() {
    let mut budget = Budget::default();
    let t0 = Instant::now();
    let quick = std::time::Duration::from_millis(100);
    let long = READY_LIFETIME;
    for _ in 0..4 {
        assert!(budget.on_exit(quick, t0).is_some());
        budget.restarted(t0);
    }
    // One that lived: the count starts over, and its own restarts are judged by the window.
    assert_eq!(budget.on_exit(long, t0), Some(RESTART_BACKOFFS[0]));
    budget.restarted(t0);
    for _ in 0..5 {
        assert!(budget.on_exit(quick, t0).is_some());
        budget.restarted(t0);
    }
    assert!(budget.on_exit(quick, t0).is_none(), "five never-ready restarts since the ready one");
    let mut budget = Budget::default();
    for i in 0..MAX_RESTARTS_PER_WINDOW {
        assert!(budget.on_exit(long, t0 + std::time::Duration::from_secs(i as u64)).is_some());
        budget.restarted(t0 + std::time::Duration::from_secs(i as u64));
    }
    assert!(budget.on_exit(long, t0 + std::time::Duration::from_secs(10)).is_none(), "five ready restarts in the window");
    assert!(
        budget.on_exit(long, t0 + RESTART_WINDOW + std::time::Duration::from_secs(10)).is_some(),
        "the window slid past them"
    );
}
