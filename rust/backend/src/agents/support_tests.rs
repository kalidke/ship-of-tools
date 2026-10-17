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

/// Two separate facts about the machine running the test: its token can make a symlink, and it can cross a junction
/// it made (a process that enforces redirection trust cannot, os error 448). Probed in a temp folder.
#[derive(Clone, Copy, Debug)]
pub(super) struct LinkFacts {
    pub symlink: bool,
    pub crosses_junction: bool,
}

pub(super) fn link_facts() -> LinkFacts {
    #[cfg(unix)]
    return LinkFacts { symlink: true, crosses_junction: true };
    #[cfg(windows)]
    {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("t");
        std::fs::create_dir(&target).unwrap();
        let symlink = std::os::windows::fs::symlink_dir(&target, tmp.path().join("s")).is_ok();
        let junction = tmp.path().join("j");
        let made = super::account_links::make_junction(&target, &junction).is_ok();
        let crosses_junction = made && std::fs::metadata(&junction).is_ok();
        let _ = std::fs::remove_dir(&junction);
        LinkFacts { symlink, crosses_junction }
    }
}

/// Assert `result` and the account folder show the form `link_facts` predicts: symlinks to the default's entries,
/// junctions for `folders` and copies for `files`, or a refusal naming the untraversable junction with nothing made.
pub(super) fn assert_shared_as_the_box_allows(
    result: &Result<(), String>,
    default_dir: &Path,
    account_dir: &Path,
    folders: &[&str],
    files: &[&str],
) {
    let facts = link_facts();
    match (facts.symlink, facts.crosses_junction) {
        (false, false) => {
            let msg = result.as_ref().unwrap_err();
            assert!(msg.contains("cannot be traversed"), "{msg}");
            for name in folders.iter().chain(files) {
                assert!(std::fs::symlink_metadata(account_dir.join(name)).is_err(), "{name}: a refused call leaves nothing");
            }
        }
        (symlinks, _) => {
            result.as_ref().unwrap();
            for name in folders.iter().chain(files) {
                let entry = account_dir.join(name);
                let meta = std::fs::symlink_metadata(&entry).unwrap();
                let shared = default_dir.join(name);
                if symlinks || folders.contains(name) {
                    assert!(meta.file_type().is_symlink(), "{name} should be a link ({facts:?})");
                    assert_eq!(std::fs::read_link(&entry).unwrap(), shared, "{name} should point at the default's entry");
                } else {
                    assert!(!meta.file_type().is_symlink(), "{name} should be a copy");
                    assert_eq!(std::fs::read(&entry).unwrap(), std::fs::read(&shared).unwrap());
                }
            }
        }
    }
}
