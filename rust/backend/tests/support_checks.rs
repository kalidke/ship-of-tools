#![cfg(target_os = "linux")]
//! The shared support's own answers: the command it gives a daemon names the Julia binary the runner found.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

/// The command the support gives a daemon hands it the Julia binary `julia_binary` names, as `SOT_JULIA_BIN`, and no
/// `SOT_JULIA_BIN` when none is found.
#[test]
fn a_daemon_is_handed_the_julia_binary_the_runner_found() {
    let given = support::sotd_command()
        .get_envs()
        .find(|(name, _)| *name == "SOT_JULIA_BIN")
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned());
    assert_eq!(given, support::julia_binary());
}
