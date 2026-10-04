//! Test support for the row store: the env guard that serializes and restores the process env.

// `app_config_dir()`'s platform dispatch, and the one-time Windows
// migration off the old HOME-derived root. Serialized under the
// crate-wide `paths::ENV_TEST_LOCK` (Codex review, PR #175: a
// module-local mutex here couldn't stop a test in THIS module from
// racing a `paths.rs` test over the same env vars).

pub(super) struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    localappdata: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
    system_drive: Option<std::ffi::OsString>,
    // Snapshotted/restored too (not just set) because a few tests below
    // pin it to a known value so `migrate_legacy_state_dirs`'s
    // `declared_host()` calls resolve deterministically — the real
    // hostname would otherwise leak into the "-<host>" suffix these
    // tests assert on, and a leaked value would poison every other
    // `declared_host()`-reading test sharing this crate-wide lock.
    sot_self_host: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, val) in [
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("HOME", &self.home),
            ("LOCALAPPDATA", &self.localappdata),
            ("USERPROFILE", &self.userprofile),
            ("SystemDrive", &self.system_drive),
            ("SOT_SELF_HOST", &self.sot_self_host),
        ] {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

pub(super) fn env_guarded() -> EnvGuard {
    let serial = crate::paths::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    EnvGuard {
        xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
        home: std::env::var_os("HOME"),
        localappdata: std::env::var_os("LOCALAPPDATA"),
        userprofile: std::env::var_os("USERPROFILE"),
        system_drive: std::env::var_os("SystemDrive"),
        sot_self_host: std::env::var_os("SOT_SELF_HOST"),
        _serial: serial,
    }
}
