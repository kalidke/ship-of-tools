//! The daemon's half of messaging: mail delivery and the address book.

pub(crate) mod mail;
pub(crate) mod registry;
pub(crate) mod wake;

use std::path::PathBuf;

/// The sot-comm home directory (`~/.sot-comm` by default) — ONE resolver
/// (Codex round finding 8), shared by the daemon's own registry reads
/// (`comm::registry::registry::comm_registry_path`, used on every platform for
/// `workspace.list`) and the env it hands a spawned capsule
/// (`agents::env::capsule_supervisor_env`, which ALSO injects
/// `SOT_COMM_HOME` explicitly into the child so the scripts and the
/// daemon can never resolve two different homes for the same box).
/// `SOT_COMM_HOME` wins outright when set (an explicit override, used
/// verbatim); else `$HOME/.sot-comm` (`comm-lib.sh`'s own default,
/// matches on every platform where a shell set `$HOME`); else — Windows
/// in practice, since `USERPROFILE` is normally unset elsewhere —
/// `%USERPROFILE%\.sot-comm`. Genuinely cross-platform (unlike
/// `windows_state_root` above): a Linux/macOS daemon reads the SAME
/// comm registry a Unix comm session writes, so this is never
/// `#[cfg(windows)]`-gated. `None` only when none of the three are set. `comm-lib.sh`'s `COMM_HOME` is the
/// same rule in shell (`$HOME` is the user profile under Git Bash).
pub(crate) fn sot_comm_home() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("SOT_COMM_HOME") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    if let Some(h) = std::env::var_os("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h).join(".sot-comm"));
        }
    }
    if let Some(u) = std::env::var_os("USERPROFILE") {
        if !u.is_empty() {
            return Some(PathBuf::from(u).join(".sot-comm"));
        }
    }
    None
}

#[cfg(test)]
mod sot_comm_home_tests {
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
}
