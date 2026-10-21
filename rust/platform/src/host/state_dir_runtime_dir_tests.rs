use super::runtime_dir;
use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    sot_runtime_dir: Option<std::ffi::OsString>,
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.sot_runtime_dir.take() {
            Some(v) => std::env::set_var("SOT_RUNTIME_DIR", v),
            None => std::env::remove_var("SOT_RUNTIME_DIR"),
        }
    }
}
fn guarded() -> EnvGuard {
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    EnvGuard {
        sot_runtime_dir: std::env::var_os("SOT_RUNTIME_DIR"),
        _serial: serial,
    }
}

#[test]
fn a_private_sot_runtime_dir_wins_over_discovery() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-log-runtime-dir-test-private-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("SOT_RUNTIME_DIR", &dir);
    assert_eq!(runtime_dir().unwrap(), dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_private_sot_runtime_dir_is_a_loud_permission_denied() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-log-runtime-dir-test-world-readable-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("SOT_RUNTIME_DIR", &dir);
    let err = runtime_dir().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        err.to_string().contains("SOT_RUNTIME_DIR"),
        "error should name the offending var: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn falls_back_to_discovery_when_unset() {
    let _guard = guarded();
    std::env::remove_var("SOT_RUNTIME_DIR");
    assert_eq!(runtime_dir().unwrap(), super::runtime_sot_dir());
}

#[test]
fn a_relative_sot_runtime_dir_is_a_loud_invalid_input() {
    // Codex round finding 4: a relative override resolves against
    // whatever the CALLER's own current directory happens to be at
    // the moment -- a second, silent source of disagreement this
    // propagation seam exists to remove. Rejected before the
    // private-dir check even runs (never a partial pass on some
    // accidentally-private relative path).
    let _guard = guarded();
    std::env::set_var("SOT_RUNTIME_DIR", "relative/sot/dir");
    let err = runtime_dir().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("SOT_RUNTIME_DIR"),
        "error should name the offending var: {err}"
    );
}
