use super::*;
use sot_log::capsule::producer::ExitStatus;
use sot_log::capsule::{ExitKind, ExitSummary};

fn summary(exit_code: Option<ExitStatus>) -> sot_log::Result<ExitSummary> {
    Ok(ExitSummary {
        exit_code,
        exit_kind: ExitKind::ProducerExited,
        frames_written: 0,
        segments_sealed: 0,
        handshake_answered: false,
        handshake_suppressed_matches: 0,
        resize_os_calls: 0,
    })
}

#[test]
fn leg_exit_code_reserves_71_for_storage_exhaustion() {
    #[cfg(unix)]
    let (full, other) = (libc::ENOSPC, libc::EIO);
    #[cfg(windows)]
    let (full, other) = (112, 1117);
    let io = |code| std::io::Error::from_raw_os_error(code);
    let transport = sot_log::lane::transport::TransportError::RuntimeDir(io(full));
    assert_eq!(leg_exit_code(&Err(sot_log::Error::Io(io(full)))), 71);
    // A full runtime folder is not the state root's volume being full.
    assert_eq!(leg_exit_code(&Err(sot_log::Error::Transport(transport))), 1);
    assert_eq!(leg_exit_code(&Err(sot_log::Error::Io(io(other)))), 1);
    assert_eq!(leg_exit_code(&summary(Some(ExitStatus::Code(71)))), 1);
    assert_eq!(leg_exit_code(&summary(Some(ExitStatus::Code(3)))), 3);
    assert_eq!(leg_exit_code(&summary(Some(ExitStatus::Signal(9)))), 137);
    assert_eq!(leg_exit_code(&summary(None)), 1);
}

#[test]
fn supervisor_lock_facade_is_reachable_and_works_from_this_binary_crate() {
    let dir = tempfile::tempdir().unwrap();
    let guard = sot_log::supervisor::journal::fence::lock_supervisor(dir.path()).unwrap();
    assert!(sot_log::supervisor::journal::fence::supervisor_lock_path(dir.path()).is_file());
    drop(guard);
}
