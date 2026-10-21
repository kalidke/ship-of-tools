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
    for code in [Some(1), Some(2), Some(-1), None] {
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
