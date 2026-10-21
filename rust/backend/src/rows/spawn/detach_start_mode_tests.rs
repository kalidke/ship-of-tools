    use super::*;

#[test]
fn only_a_resume_by_id_is_first_leg_only() {
    let ordinary: Vec<String> = ["claude", "--permission-mode", "auto", "/sot-session-start"].map(String::from).to_vec();
    assert!(first_leg_only(&ordinary).is_empty());
    let reauth: Vec<String> =
        ["claude", "--permission-mode", "auto", "--resume", "abc", "/sot-session-start"].map(String::from).to_vec();
    assert_eq!(first_leg_only(&reauth), ["--first-leg-only", "--resume", "--first-leg-only", "abc"]);
}

#[test]
fn mode_flag_matches_the_sot_capsule_cli() {
    assert_eq!(mode_flag(StartMode::Start), "--start");
    assert_eq!(mode_flag(StartMode::Resume), "--resume");
}
