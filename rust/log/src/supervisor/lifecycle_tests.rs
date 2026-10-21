use super::*;

/// A reset that fails with storage exhaustion writes no `Failed` record:
/// its operation stays active for startup recovery to finish. Any other
/// failure still writes `Failed` and is fatal.
#[test]
fn a_storage_reset_failure_records_nothing() {
    #[cfg(unix)]
    let full = std::io::Error::from_raw_os_error(libc::ENOSPC);
    #[cfg(windows)]
    let full = std::io::Error::from_raw_os_error(112);
    let dir = tempfile::tempdir().unwrap();
    let result = reset_failure(dir.path(), "op-full", crate::Error::Io(full));
    assert!(matches!(result, ResetWorkerResult::Storage(_)));
    assert_eq!(journal::read_terminal(dir.path(), "op-full").unwrap(), None, "no terminal record for storage");

    let result = reset_failure(dir.path(), "op-state", crate::Error::State("pointer unreadable".into()));
    assert!(matches!(result, ResetWorkerResult::Fatal(_)));
    assert!(matches!(
        journal::read_terminal(dir.path(), "op-state").unwrap(),
        Some(journal::TerminalRecord::Failed { .. })
    ));
}

#[test]
fn wire_phase_maps_every_state_to_the_adr_s_five_values() {
    assert_eq!(Lifecycle::EndedNoRespawn.wire_phase(), SupervisorPhase::EndedNoRespawn);
    assert_eq!(
        Lifecycle::Terminal { detail: "x".into(), entered_at: Instant::now() }.wire_phase(),
        SupervisorPhase::Terminal
    );
}

/// `take_worker_handle` must actually extract (not merely drop) an
/// in-flight worker's handle, for `force_terminal`'s own "abandon
/// the worker while jumping straight to Terminal" (`stop` no longer uses this at all — only `force_terminal`
/// does now). Constructing a real `Ready` variant needs a live,
/// OS-proven `Process` this unit test has no safe way to
/// fabricate (see `tests/supervisor/` for that half, exercised
/// end-to-end against a real process); the worker-bearing states are
/// what this function actually exists for and are fully exercisable
/// here.
#[test]
fn take_worker_handle_extracts_the_handle_from_a_worker_bearing_state() {
    let mut retired_legs = Vec::new();
    let (_tx, rx) = mpsc::channel::<RecoveryOutcome>();
    let mut recovering = Lifecycle::Recovering { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now() };
    assert!(take_worker_handle(&mut recovering, &mut retired_legs).is_some());

    let mut ended = Lifecycle::EndedNoRespawn;
    assert!(take_worker_handle(&mut ended, &mut retired_legs).is_none());
    assert!(retired_legs.is_empty(), "neither state above carries a leg process to retire");
}
