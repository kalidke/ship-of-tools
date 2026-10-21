use super::*;

#[test]
fn a_built_value_answers_only_for_what_it_holds() {
    let e = Env::empty().with("A", "1").with("B", "");
    assert_eq!(e.get("A"), Some(OsStr::new("1")));
    assert_eq!(e.get("B"), Some(OsStr::new("")));
    assert_eq!(e.get_nonempty("B"), None);
    assert_eq!(e.get("C"), None);
    assert_eq!(e.clone().without("A").get("A"), None);
    assert_eq!(e.text("A"), "1");
    assert_eq!(e.text("C"), "");
}

#[test]
fn the_process_snapshot_is_a_copy() {
    let e = Env::process();
    assert_eq!(e.get("PATH").is_some(), std::env::var_os("PATH").is_some());
    let changed = e.clone().with("SOT_ENV_TEST_ONLY", "x");
    assert!(std::env::var_os("SOT_ENV_TEST_ONLY").is_none() && changed.get("SOT_ENV_TEST_ONLY").is_some());
}
