//! The per-machine Ship of Tools state directory (ADR 0041 step 6, unit
//! U0 — promoted from the frontend's own `paths.rs`, build-order step 1's
//! comment there): ONE resolution rule, owned here so every process that
//! needs it shares it rather than drifting copies. That drift already
//! happened once — `gpu.rs`'s old `sot_state_dir()` checked
//! `%LOCALAPPDATA%` only on Windows while `state.rs`'s old `state_path()`
//! checked `$XDG_STATE_HOME` first on *both* platforms, so a Windows box
//! with `XDG_STATE_HOME` set split session state from the relaunch
//! sentinel into two different directories.
//!
//! `%LOCALAPPDATA%\sot` wins on Windows because the launcher scripts
//! (`scripts/launch-sot.ps1`, `scripts/relaunch-sot.ps1`) write the
//! relaunch sentinel and staged binary under `%LOCALAPPDATA%\sot`
//! unconditionally, and this crate's own Windows-side voyage/supervisor
//! state (ADR 0041) is pinned to the same subtree — so a second env var
//! winning on Windows would let the FE's resume state and the capsule's
//! durable state disagree about which machine-local directory is
//! authoritative.
//!
//! Pure refactor: this is a byte-for-byte port of the frontend's own
//! resolution logic (no behavior change), which now DELEGATES to this
//! function instead of carrying its own copy.

/// Resolve the per-machine state directory: `%LOCALAPPDATA%\sot` (falling
/// back to `%USERPROFILE%\AppData\Local\sot` — the same location
/// `%LOCALAPPDATA%` names, derived directly for the rare login where the
/// env var itself is unset or empty) on Windows, `$XDG_STATE_HOME/sot` (or
/// `$HOME/.local/state/sot`) elsewhere. Home for the staged binary + logs
/// (ADR 0017), the relaunch sentinel, the FE control channel (ADR 0019),
/// session reconnect memory, and — from ADR 0041 on — `drawer.voyage`/
/// `supervisor.lock` and the capsule's own durable state.
///
/// No POSIX (`XDG_*`/`HOME`/`/tmp`) fallback exists on Windows — deriving
/// a `$HOME`-shaped path there depends on which shell launched the
/// process (a git-bash shell exports `HOME`, PowerShell doesn't), which
/// is exactly the class of bug this whole resolver exists to close (see
/// this module's own doc for the FE/capsule drift it already caused
/// once). `None` on Windows only when NEITHER `%LOCALAPPDATA%` nor
/// `%USERPROFILE%` is set — not a normal Windows login; callers there
/// should treat it as a hard failure (see e.g.
/// `sot-backend`'s `paths::windows_state_root`), not a silent fallback.
/// `None` elsewhere only when neither `$XDG_STATE_HOME` nor `$HOME` is
/// set.
pub fn sot_state_dir() -> Option<std::path::PathBuf> {
    let dir = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .filter(|v| !v.is_empty())
                    .map(|home| std::path::PathBuf::from(home).join("AppData").join("Local"))
            })
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local").join("state"))
            })
    }?;
    Some(dir.join("sot"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Every test here mutates process-global env vars, and `cargo test`
    // runs tests in parallel within one process by default — the SAME
    // discipline `state_persistence.rs`'s own tests already use for this
    // exact reason.
    static SERIAL: Mutex<()> = Mutex::new(());

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
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
            home: std::env::var_os("HOME"),
            localappdata: std::env::var_os("LOCALAPPDATA"),
            userprofile: std::env::var_os("USERPROFILE"),
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
}
