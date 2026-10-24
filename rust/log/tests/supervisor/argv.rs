//! The argv grammar: a spelling the daemon no longer sends is a usage error, not a flag that is accepted and ignored.

use super::*;

/// `--survival`, `--assume-no-rollback-target` and `--first-leg-only` were retired (the daemon sends `--first-run-only`):
/// each now ends `supervise` with the usage and exit 2 before it takes the fence or binds a socket, and starts no agent.
/// The spellings are built with `concat!` so no rename of a flag rewrites them.
#[cfg(target_os = "linux")]
#[test]
fn a_retired_flag_is_a_usage_error() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let retired: [&[&str]; 3] = [
        &[concat!("--sur", "vival"), "normal"],
        &[concat!("--assume-no-", "rollback-target")],
        &[concat!("--first-", "leg-only"), "x"],
    ];
    for flag in retired {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let mut cmd = Command::new(capsule_exe());
        cmd.arg("supervise")
            .arg(&state_dir)
            .arg("--start")
            .args(flag)
            .args(["--", "true"])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let mut guard = CapsuleGuard::spawn(&mut cmd);
        let status = wait_for_exit(&mut guard, Duration::from_secs(30));
        assert_eq!(status.code(), Some(2), "{flag:?} must be a usage error, got {status:?}");
        assert!(
            !journal::fence::supervisor_lock_path(&state_dir).exists(),
            "{flag:?} left a supervisor.lock: the supervisor started before it refused the flag"
        );
    }
}
