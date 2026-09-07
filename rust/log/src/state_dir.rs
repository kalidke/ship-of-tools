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

// ---------------------------------------------------------------------
// L1-unix LU1b (ADR 0043 decision 1): the per-user RUNTIME dir — where
// live sockets/pipes go, as opposed to `sot_state_dir`'s durable-state
// tree above. Moved DOWN here from `sot-protocol`'s `session_socket`
// module (ADR 0042 L2b's original home): the daemon's own session
// sockets and the Ship's Log Unix transport (`socket_unix.rs`) both need
// the identical derivation, and `sot-log` is the lower crate in the
// dependency graph (the frontend and backend already depend on both;
// `sot-protocol` gains a dependency on `sot-log`, never the reverse).
// `session_socket.rs` now re-exports these three names verbatim so every
// existing `sot_protocol::runtime_sot_dir()` / `session_socket::
// is_private_dir(...)` / `current_uid()` call site keeps compiling
// unchanged.
// ---------------------------------------------------------------------

use std::path::PathBuf;

/// Per-user runtime root for Ship of Tools Unix sockets. Unix-only in
/// effect (Windows's own `session_socket_path` branch never calls this),
/// but left uncompiled-out so the non-Unix stub helpers below don't need
/// a matching `#[cfg]` split of their own.
///
/// NOT a pure function of (uid): it consults `$XDG_RUNTIME_DIR` and
/// filesystem state, so two same-uid processes with different
/// environments can disagree — see [`runtime_dir`] for the propagation
/// seam that makes the *actually used* runtime dir deterministic across
/// a daemon's whole process tree.
pub fn runtime_sot_dir() -> PathBuf {
    if let Some(dir) = private_xdg_runtime_dir() {
        return dir.join("sot");
    }
    let uid = current_uid();
    let run_user_dir = PathBuf::from(format!("/run/user/{uid}"));
    if is_private_dir(&run_user_dir) {
        return run_user_dir.join("sot");
    }
    PathBuf::from(format!("/tmp/sot-{uid}"))
}

/// L1-unix LU1b (ADR 0043 decision 1): the propagation seam. Determinism
/// for a Unix-domain-socket path comes from PROPAGATION, not discovery —
/// the daemon (LU4) resolves the runtime dir ONCE and exports it as
/// `SOT_RUNTIME_DIR` to every capsule and client it spawns, so they can
/// never disagree with the daemon (or each other) about `$XDG_RUNTIME_DIR`
/// the way two independently-launched processes' own [`runtime_sot_dir`]
/// discovery could. A set `SOT_RUNTIME_DIR` is trusted only after it is
/// ABSOLUTE (a relative one would resolve against whatever the CALLER's
/// own current directory happens to be at the moment — a second, silent
/// source of disagreement this propagation seam exists to remove, since
/// `chdir` is per-process state the daemon cannot pin for everything it
/// spawns) and passes the SAME [`is_private_dir`] check `runtime_sot_dir`'s
/// own discovery applies to its candidates — a stale or maliciously-set
/// var pointing at a group/world-accessible or symlinked directory is a
/// loud, named error, never a silent fallback to discovery (which would
/// let a mismatched env var produce a silent SECOND endpoint no caller
/// intended). Discovery ([`runtime_sot_dir`]) is only the fallback for a
/// process started outside the daemon's tree.
pub fn runtime_dir() -> std::io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("SOT_RUNTIME_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "SOT_RUNTIME_DIR must be an absolute path",
            ));
        }
        return if is_private_dir(&dir) {
            Ok(dir)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "SOT_RUNTIME_DIR={} is not a private, owner-only directory",
                    dir.display()
                ),
            ))
        };
    }
    Ok(runtime_sot_dir())
}

/// `$XDG_RUNTIME_DIR` if it's set, exists, and is owner-only (no group/other
/// bits). A world/group-accessible or missing runtime dir falls through to
/// the next resolution tier rather than being trusted. Split into an env
/// read (this) + a pure path check ([`is_private_dir`]) so the safety logic
/// is unit-testable against real temp dirs without mutating
/// `$XDG_RUNTIME_DIR` — a process-global env var other tests in this crate
/// touch concurrently.
#[cfg(unix)]
fn private_xdg_runtime_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    if is_private_dir(&dir) {
        Some(dir)
    } else {
        None
    }
}
#[cfg(not(unix))]
fn private_xdg_runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
}

/// `true` if `dir` exists, is a REAL directory (not a symlink to one),
/// owned by THIS process's uid, and owner-only (no group/other bits).
///
/// Uses `symlink_metadata` (lstat — does NOT follow a symlink) rather than
/// `metadata`, and checks ownership, not just mode (security review, F1: a
/// hostile local user who can write into `$XDG_RUNTIME_DIR`'s parent could
/// otherwise plant a symlink there, or — if `$XDG_RUNTIME_DIR` itself were
/// ever attacker-writable, e.g. a misconfigured shared runtime dir — an
/// attacker-owned 0700 directory, and the old `metadata`-plus-mode-only
/// check would have followed/trusted either).
#[cfg(unix)]
pub fn is_private_dir(dir: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return false;
    }
    if !meta.is_dir() {
        return false;
    }
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if meta.uid() != current_uid() {
        return false;
    }
    meta.permissions().mode() & 0o077 == 0
}
#[cfg(not(unix))]
pub fn is_private_dir(dir: &std::path::Path) -> bool {
    dir.is_dir()
}

/// Numeric uid for path derivation (`/run/user/<uid>`, `/tmp/sot-<uid>`) and
/// the ownership check in `is_private_dir`/`rust/backend`'s
/// `secure_private_dir`. `0` on non-Unix, where these callers are never
/// reached in practice (Windows sessions don't run tmux, and
/// `session_socket_path` takes the named-pipe branch without calling any
/// of this) — this function exists so the module compiles everywhere, not
/// because the value is meaningful there.
#[cfg(unix)]
pub fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments, has no preconditions, and cannot
    // fail.
    unsafe { libc::getuid() }
}
#[cfg(not(unix))]
pub fn current_uid() -> u32 {
    0
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

/// Moved verbatim from `sot-protocol`'s `session_socket` module (ADR 0043
/// decision 1, L1-unix LU1b) with `is_private_dir` itself — deliberately
/// NOT via `$XDG_RUNTIME_DIR` mutation, for the same env-var-race reason
/// noted on `private_xdg_runtime_dir`'s doc comment.
#[cfg(all(test, unix))]
mod is_private_dir_tests {
    use super::is_private_dir;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "sot-log-state-dir-test-{}-{}-{name}",
            std::process::id(),
            n
        ))
    }

    #[test]
    fn owner_only_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch_dir("owner-only");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(is_private_dir(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn group_readable_dir_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch_dir("group-readable");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(!is_private_dir(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn world_readable_dir_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch_dir("world-readable");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!is_private_dir(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_dir_is_not_private() {
        let d = scratch_dir("missing");
        assert!(!is_private_dir(&d));
    }

    #[test]
    fn a_file_is_not_a_private_dir() {
        let d = scratch_dir("a-file");
        std::fs::write(&d, b"not a dir").unwrap();
        assert!(!is_private_dir(&d));
        let _ = std::fs::remove_file(&d);
    }

    #[test]
    fn symlink_to_a_private_dir_is_rejected() {
        // The hijack case (F1): even a symlink pointing at an otherwise-
        // valid owner-only dir must be rejected — trusting it would let an
        // attacker who controls the symlink redirect us anywhere later by
        // repointing it, and `is_private_dir` must reject based on the
        // PATH's own type (lstat), not what it resolves to.
        use std::os::unix::fs::PermissionsExt;
        let target = scratch_dir("symlink-target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = scratch_dir("symlink-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!is_private_dir(&link));
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&target);
    }
}

/// L1-unix LU1b: [`runtime_dir`]'s own propagation-seam contract — a set
/// `SOT_RUNTIME_DIR` wins over discovery only once it passes
/// `is_private_dir`, and a failing one is a loud, named `PermissionDenied`,
/// never a silent fallback. Serialized against a private mutex (not
/// `tests::SERIAL`, a different module's private static) for the same
/// concurrent-env-var-mutation reason every other env-touching test module
/// in this crate uses one.
#[cfg(all(test, unix))]
mod runtime_dir_tests {
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
}
