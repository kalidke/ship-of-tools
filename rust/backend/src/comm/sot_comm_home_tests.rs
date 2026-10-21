use super::*;
use crate::paths::ENV_TEST_LOCK;

struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    sot_comm_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, val) in [
            ("SOT_COMM_HOME", &self.sot_comm_home),
            ("HOME", &self.home),
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
        sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
        home: std::env::var_os("HOME"),
        userprofile: std::env::var_os("USERPROFILE"),
        _serial: serial,
    }
}

#[test]
fn explicit_override_wins_outright() {
    let _guard = guarded();
    std::env::set_var("SOT_COMM_HOME", "/custom/comm-home");
    std::env::set_var("HOME", "/home/someone");
    std::env::set_var("USERPROFILE", r"C:\Users\someone");
    assert_eq!(sot_comm_home(), Some(PathBuf::from("/custom/comm-home")));
}

#[test]
fn falls_back_to_home_dot_sot_comm() {
    let _guard = guarded();
    std::env::remove_var("SOT_COMM_HOME");
    std::env::set_var("HOME", "/home/someone");
    assert_eq!(sot_comm_home(), Some(PathBuf::from("/home/someone/.sot-comm")));
}

#[test]
fn falls_back_to_userprofile_when_home_unset() {
    let _guard = guarded();
    std::env::remove_var("SOT_COMM_HOME");
    std::env::remove_var("HOME");
    std::env::set_var("USERPROFILE", r"C:\Users\someone");
    assert_eq!(
        sot_comm_home(),
        Some(PathBuf::from(r"C:\Users\someone").join(".sot-comm"))
    );
}

#[test]
fn none_when_nothing_is_set() {
    let _guard = guarded();
    std::env::remove_var("SOT_COMM_HOME");
    std::env::remove_var("HOME");
    std::env::remove_var("USERPROFILE");
    assert_eq!(sot_comm_home(), None);
}
