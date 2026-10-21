use super::*;
use std::sync::Mutex;

// Every test here mutates process-global env vars, and `cargo test`
// runs tests in parallel within one process by default — the SAME
// discipline `ui/persist/resume.rs`'s own tests already use for this
// exact reason.
static SERIAL: Mutex<()> = Mutex::new(());

struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    localappdata: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
    sot_self_host: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, val) in [
            ("XDG_STATE_HOME", &self.xdg_state_home),
            ("HOME", &self.home),
            ("LOCALAPPDATA", &self.localappdata),
            ("USERPROFILE", &self.userprofile),
            ("SOT_SELF_HOST", &self.sot_self_host),
        ] {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn guarded() -> EnvGuard {
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    EnvGuard {
        xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
        home: std::env::var_os("HOME"),
        localappdata: std::env::var_os("LOCALAPPDATA"),
        userprofile: std::env::var_os("USERPROFILE"),
        sot_self_host: std::env::var_os("SOT_SELF_HOST"),
        _serial: serial,
    }
}

// `cfg!(windows)` is a compile-time constant, so each platform's own
// branch is only exercisable by a test run that actually targets it —
// the non-Windows precedence below runs on Linux/macOS CI; the
// LOCALAPPDATA precedence further down runs on Windows CI. Neither
// side had ANY test coverage before this module existed (the
// pre-existing frontend function had none).
#[test]
#[cfg(not(windows))]
fn xdg_state_home_wins_over_the_home_fallback() {
    let _guard = guarded();
    std::env::set_var("XDG_STATE_HOME", "/xdg-state");
    std::env::set_var("HOME", "/home/someone");
    assert_eq!(sot_state_dir(), Some(std::path::PathBuf::from("/xdg-state/sot")));
}

#[test]
#[cfg(not(windows))]
fn home_fallback_used_when_xdg_state_home_is_unset() {
    let _guard = guarded();
    std::env::remove_var("XDG_STATE_HOME");
    std::env::set_var("HOME", "/home/someone");
    assert_eq!(
        sot_state_dir(),
        Some(std::path::PathBuf::from("/home/someone/.local/state/sot"))
    );
}

#[test]
#[cfg(not(windows))]
fn none_when_neither_var_is_set() {
    let _guard = guarded();
    std::env::remove_var("XDG_STATE_HOME");
    std::env::remove_var("HOME");
    assert_eq!(sot_state_dir(), None);
}

#[test]
#[cfg(windows)]
fn localappdata_is_used_on_windows() {
    let _guard = guarded();
    std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
    assert_eq!(
        sot_state_dir(),
        Some(std::path::PathBuf::from(r"C:\Users\someone\AppData\Local\sot"))
    );
}

#[test]
#[cfg(windows)]
fn windows_ignores_xdg_state_home() {
    let _guard = guarded();
    std::env::set_var("XDG_STATE_HOME", r"C:\should\be\ignored");
    std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
    assert_eq!(
        sot_state_dir(),
        Some(std::path::PathBuf::from(r"C:\Users\someone\AppData\Local\sot"))
    );
}

#[test]
#[cfg(windows)]
fn windows_falls_back_to_userprofile_when_localappdata_unset() {
    let _guard = guarded();
    std::env::remove_var("LOCALAPPDATA");
    std::env::set_var("USERPROFILE", r"C:\Users\someone");
    assert_eq!(
        sot_state_dir(),
        Some(std::path::PathBuf::from(r"C:\Users\someone\AppData\Local\sot"))
    );
}

#[test]
#[cfg(windows)]
fn windows_falls_back_to_userprofile_when_localappdata_is_empty() {
    let _guard = guarded();
    std::env::set_var("LOCALAPPDATA", "");
    std::env::set_var("USERPROFILE", r"C:\Users\someone");
    assert_eq!(
        sot_state_dir(),
        Some(std::path::PathBuf::from(r"C:\Users\someone\AppData\Local\sot"))
    );
}

#[test]
#[cfg(windows)]
fn none_on_windows_without_localappdata_or_userprofile() {
    let _guard = guarded();
    std::env::remove_var("LOCALAPPDATA");
    std::env::remove_var("USERPROFILE");
    assert_eq!(sot_state_dir(), None);
}

#[test]
fn host_name_prefers_sot_self_host_when_set() {
    let _guard = guarded();
    std::env::set_var("SOT_SELF_HOST", "test-host");
    assert_eq!(host_name(), Ok("test-host".to_string()));
}

#[test]
fn host_name_takes_sot_self_host_verbatim_uncased() {
    // An explicit override is trusted as given -- unlike the
    // gethostname fallback below, it is never lowercased.
    let _guard = guarded();
    std::env::set_var("SOT_SELF_HOST", "Test-Host");
    assert_eq!(host_name(), Ok("Test-Host".to_string()));
}

#[test]
fn host_name_falls_back_to_gethostname_when_sot_self_host_is_empty() {
    let _guard = guarded();
    std::env::set_var("SOT_SELF_HOST", "");
    let name = host_name().expect("gethostname fallback must resolve");
    assert_eq!(name, name.to_lowercase(), "fallback must be lowercased");
    assert!(!name.contains('.'), "fallback must be the first label only");
}

#[test]
fn host_name_falls_back_to_gethostname_when_sot_self_host_is_unset() {
    let _guard = guarded();
    std::env::remove_var("SOT_SELF_HOST");
    assert!(host_name().is_ok(), "a real host always has a gethostname label");
}
