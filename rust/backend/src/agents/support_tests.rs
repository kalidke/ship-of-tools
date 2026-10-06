// support_tests.rs — fixtures both account test modules share.

use std::path::{Path, PathBuf};

pub(super) fn touch_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
}
pub(super) fn touch_file(path: &Path) {
    std::fs::write(path, b"").unwrap();
}

/// The spelling the platform itself would hand a child. Not a `#[test]`.
pub(super) fn platform_spelling(p: &std::path::Path) -> PathBuf {
    crate::paths::simplify_verbatim(p.canonicalize().expect("canonical tempdir"))
}

// Serialized under the crate-wide `paths::ENV_TEST_LOCK` (mirrors
// `rows/store/support_tests.rs`'s own `EnvGuard` exactly — see that module's
// comment: `cargo test` runs in parallel within one process, and
// several modules' resolvers read the SAME env vars, HOME included).
pub(super) struct SelfFileEnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    home: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
    sot_comm_home: Option<std::ffi::OsString>,
    sot_self_host: Option<std::ffi::OsString>,
    /// Added for the auto-memory tests, which need it ABSENT (the daemon's
    /// own env carries none) or set to a fixture dir. Guarded here rather
    /// than saved and restored by hand at the call site, because a hand-
    /// rolled restore is skipped when an assertion fires between the set
    /// and the restore, leaving the variable set for every later test in
    /// the process.
    claude_config_dir: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    localappdata: Option<std::ffi::OsString>,
}

impl Drop for SelfFileEnvGuard {
    fn drop(&mut self) {
        for (key, val) in [
            ("HOME", &self.home),
            ("USERPROFILE", &self.userprofile),
            ("SOT_COMM_HOME", &self.sot_comm_home),
            ("SOT_SELF_HOST", &self.sot_self_host),
            ("CLAUDE_CONFIG_DIR", &self.claude_config_dir),
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("LOCALAPPDATA", &self.localappdata),
        ] {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

pub(super) fn self_file_env_guarded() -> SelfFileEnvGuard {
    let serial = crate::paths::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    SelfFileEnvGuard {
        home: std::env::var_os("HOME"),
        userprofile: std::env::var_os("USERPROFILE"),
        sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
        sot_self_host: std::env::var_os("SOT_SELF_HOST"),
        claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR"),
        xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
        localappdata: std::env::var_os("LOCALAPPDATA"),
        _serial: serial,
    }
}
