use std::path::PathBuf;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    xdg_state: Option<std::ffi::OsString>,
    #[cfg(windows)]
    local_app_data: Option<std::ffi::OsString>,
    #[cfg(windows)]
    user_profile: Option<std::ffi::OsString>,
    dir: PathBuf,
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.xdg_state.take() {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        #[cfg(windows)]
        {
            match self.local_app_data.take() {
                Some(v) => std::env::set_var("LOCALAPPDATA", v),
                None => std::env::remove_var("LOCALAPPDATA"),
            }
            match self.user_profile.take() {
                Some(v) => std::env::set_var("USERPROFILE", v),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(crate) fn set_test_env() -> EnvGuard {
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!(
        "sot-state-migration-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let g = EnvGuard {
        _serial,
        xdg_state: std::env::var_os("XDG_STATE_HOME"),
        #[cfg(windows)]
        local_app_data: std::env::var_os("LOCALAPPDATA"),
        #[cfg(windows)]
        user_profile: std::env::var_os("USERPROFILE"),
        dir: dir.clone(),
    };
    std::env::set_var("XDG_STATE_HOME", &dir);
    // On Windows the state dir resolves LOCALAPPDATA, then USERPROFILE.
    #[cfg(windows)]
    {
        std::env::set_var("LOCALAPPDATA", &dir);
        std::env::set_var("USERPROFILE", &dir);
    }
    g
}
