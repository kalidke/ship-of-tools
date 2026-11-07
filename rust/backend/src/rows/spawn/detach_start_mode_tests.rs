    use super::*;

#[test]
fn only_a_resume_by_id_is_first_run_only() {
    let ordinary: Vec<String> = ["claude", "--permission-mode", "auto", "/sot-session-start"].map(String::from).to_vec();
    assert!(first_run_only(&ordinary).is_empty());
    let reauth: Vec<String> =
        ["claude", "--permission-mode", "auto", "--resume", "abc", "/sot-session-start"].map(String::from).to_vec();
    assert_eq!(first_run_only(&reauth), ["--first-run-only", "--resume", "--first-run-only", "abc"]);
}

#[test]
fn mode_flag_matches_the_sot_capsule_cli() {
    assert_eq!(mode_flag(StartMode::Start), "--start");
    assert_eq!(mode_flag(StartMode::Resume), "--resume");
}

#[test]
fn the_scrub_comes_before_the_rows_pairs_so_the_row_keeps_its_selection_file() {
    let os = |s: &str| std::ffi::OsString::from(s);
    let inherited = [("SOT_SELECTION_FILE", "/outer/selection.json"), ("CLAUDECODE", "1"), ("KEEP", "x")]
        .map(|(k, v)| (os(k), os(v)))
        .into();
    let row = [("SOT_SELECTION_FILE".to_string(), "/row/selection.json".to_string())];
    let env: std::collections::BTreeMap<_, _> = leg_env(inherited, &row, &[]).into_iter().collect();
    assert_eq!(env[&os("SOT_SELECTION_FILE")], os("/row/selection.json"));
    assert!(!env.contains_key(&os("CLAUDECODE")));
    assert_eq!(env[&os("KEEP")], os("x"));
    // A daemon whose own environment had one, and a row that names none, leaves the leg with none.
    let inherited = [(os("SOT_SELECTION_FILE"), os("/outer/selection.json"))].into();
    assert!(leg_env(inherited, &[], &[]).is_empty());
}
