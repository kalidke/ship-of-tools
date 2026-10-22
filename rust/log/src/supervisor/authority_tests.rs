//! `Shared`'s own unit tests.
use super::*;

/// A supervisor that goes terminal with an agent running ends that agent: the run's channel would otherwise just be
/// dropped (a disconnect is not a kill), leaving a healthy agent under a supervisor that can no longer serve its row.
#[test]
fn a_terminal_supervisor_ends_its_running_agent() {
    let shared = Shared::new(std::env::temp_dir());
    let (kill, commands) = mpsc::channel();
    assert!(shared.run_begins(kill, Arc::new(RunSignals::default())));
    shared.terminal("the supervisor lane failed".into());
    assert!(
        matches!(commands.try_recv(), Ok(Command::Stop) | Ok(Command::Kill)),
        "the run was told to end before it was dropped"
    );
}
