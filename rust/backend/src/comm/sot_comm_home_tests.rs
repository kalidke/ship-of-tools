use super::*;

#[test]
fn explicit_override_wins_outright() {
    let env = Env::empty().with("SOT_COMM_HOME", "/custom/comm-home").with("HOME", "/home/someone").with("USERPROFILE", r"C:\Users\someone");
    assert_eq!(sot_comm_home_in(&env), Some(PathBuf::from("/custom/comm-home")));
}

#[test]
fn falls_back_to_home_dot_sot_comm() {
    let env = Env::empty().with("HOME", "/home/someone");
    assert_eq!(sot_comm_home_in(&env), Some(PathBuf::from("/home/someone/.sot-comm")));
}

#[test]
fn falls_back_to_userprofile_when_home_unset() {
    let env = Env::empty().with("USERPROFILE", r"C:\Users\someone");
    assert_eq!(sot_comm_home_in(&env), Some(PathBuf::from(r"C:\Users\someone").join(".sot-comm")));
}

#[test]
fn none_when_nothing_is_set() {
    assert_eq!(sot_comm_home_in(&Env::empty()), None);
}

#[test]
fn an_empty_variable_is_unset() {
    let env = Env::empty().with("SOT_COMM_HOME", "").with("HOME", "/home/someone");
    assert_eq!(sot_comm_home_in(&env), Some(PathBuf::from("/home/someone/.sot-comm")));
}
