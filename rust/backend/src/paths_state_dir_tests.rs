use super::*;

struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    localappdata: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, val) in [
            ("XDG_STATE_HOME", &self.xdg_state_home),
            ("HOME", &self.home),
            ("LOCALAPPDATA", &self.localappdata),
            ("USERPROFILE", &self.userprofile),
        ] {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn guarded() -> EnvGuard {
    let serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    EnvGuard {
        xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
        home: std::env::var_os("HOME"),
        localappdata: std::env::var_os("LOCALAPPDATA"),
        userprofile: std::env::var_os("USERPROFILE"),
        _serial: serial,
    }
}

#[test]
#[cfg(not(windows))]
fn unix_still_prefers_xdg_state_home() {
    let _guard = guarded();
    std::env::set_var("XDG_STATE_HOME", "/xdg-state");
    std::env::set_var("HOME", "/home/someone");
    assert_eq!(state_dir(), PathBuf::from("/xdg-state/sot"));
}

#[test]
#[cfg(not(windows))]
fn unix_falls_back_to_home_when_xdg_state_home_unset() {
    let _guard = guarded();
    std::env::remove_var("XDG_STATE_HOME");
    std::env::set_var("HOME", "/home/someone");
    assert_eq!(state_dir(), PathBuf::from("/home/someone/.local/state/sot"));
}

#[test]
#[cfg(windows)]
fn windows_uses_localappdata_state_subdir() {
    let _guard = guarded();
    std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
    assert_eq!(
        state_dir(),
        PathBuf::from(r"C:\Users\someone\AppData\Local\sot\state")
    );
}

#[test]
#[cfg(windows)]
fn windows_ignores_xdg_state_home() {
    let _guard = guarded();
    std::env::set_var("XDG_STATE_HOME", r"C:\should\be\ignored");
    std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
    assert_eq!(
        state_dir(),
        PathBuf::from(r"C:\Users\someone\AppData\Local\sot\state")
    );
}

#[test]
#[cfg(windows)]
fn windows_falls_back_to_userprofile_when_localappdata_unset() {
    let _guard = guarded();
    std::env::remove_var("LOCALAPPDATA");
    std::env::remove_var("XDG_STATE_HOME");
    std::env::set_var("USERPROFILE", r"C:\Users\someone");
    assert_eq!(
        state_dir(),
        PathBuf::from(r"C:\Users\someone\AppData\Local\sot\state")
    );
}

#[test]
#[cfg(windows)]
#[should_panic(expected = "cannot resolve the Windows state root")]
fn windows_panics_when_localappdata_and_userprofile_are_both_unset() {
    let _guard = guarded();
    std::env::remove_var("LOCALAPPDATA");
    std::env::remove_var("USERPROFILE");
    let _ = state_dir();
}
