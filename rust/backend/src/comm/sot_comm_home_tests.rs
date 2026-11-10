use super::*;

#[test]
fn explicit_override_wins_outright() {
    let env = Env::empty().with("SOT_COMM_HOME", "/custom/comm-home").with("HOME", "/home/someone").with("USERPROFILE", r"C:\Users\someone");
    assert_eq!(comm_home_rule(&env), Some(PathBuf::from("/custom/comm-home")));
}

#[test]
fn falls_back_to_home_dot_sot_comm() {
    let env = Env::empty().with("HOME", "/home/someone");
    assert_eq!(comm_home_rule(&env), Some(PathBuf::from("/home/someone/.sot-comm")));
}

#[test]
fn falls_back_to_userprofile_when_home_unset() {
    let env = Env::empty().with("USERPROFILE", r"C:\Users\someone");
    assert_eq!(comm_home_rule(&env), Some(PathBuf::from(r"C:\Users\someone").join(".sot-comm")));
}

#[test]
fn none_when_nothing_is_set() {
    assert_eq!(comm_home_rule(&Env::empty()), None);
}

#[test]
fn an_empty_variable_is_unset() {
    let env = Env::empty().with("SOT_COMM_HOME", "").with("HOME", "/home/someone");
    assert_eq!(comm_home_rule(&env), Some(PathBuf::from("/home/someone/.sot-comm")));
}

const GUARD_TEST: &str = "comm::sot_comm_home_tests::a_test_resolving_a_comm_folder_it_did_not_make_aborts_by_name";

/// The guard aborts the process, so the check runs in a child of this test binary (the parent sets the child's
/// environment): the child resolves a folder outside the temporary folder and must die with the guard's line.
#[test]
fn a_test_resolving_a_comm_folder_it_did_not_make_aborts_by_name() {
    if std::env::var_os("SOT_COMM_GUARD_CHILD").is_some() {
        sot_comm_home_in(&Env::empty().with("SOT_COMM_HOME", "/not/a/scratch/comm"));
        return;
    }
    #[allow(clippy::disallowed_methods, reason = "a bounded child of this test binary, ended by its own abort")]
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", GUARD_TEST, "--nocapture"])
        .env("SOT_COMM_GUARD_CHILD", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("start the guard's child");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the child must abort: {:?}\n{err}", out.status);
    assert!(
        err.contains("comm-home guard: test") && err.contains(GUARD_TEST) && err.contains("/not/a/scratch/comm"),
        "the guard names the test and the path: {err}"
    );
}

#[test]
fn a_comm_folder_beneath_the_temporary_folder_resolves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = Env::empty().with("SOT_COMM_HOME", dir.path().join("comm"));
    assert_eq!(sot_comm_home_in(&env), Some(dir.path().join("comm")));
}
