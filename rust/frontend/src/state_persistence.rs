// state_persistence.rs — frontend-side "where we left off" durable state
// per ADR 0013 §"Startup — resume, don't land".
//
// Two files live under `$XDG_CONFIG_HOME/sot/` (or `$HOME/.config/sot/`):
//
//   state-<hostname>.toml — this file. Global, frontend-local: last mode,
//   last-attached backend session. Hostname-suffixed because Linux
//   $HOME is shared across the Linux cohort and three
//   frontends would otherwise stomp each other's resume state.
//
//   sessions/<id>.toml — owned mostly by the backend (the [backend]
//   section is stamped by `session_state` over there); the frontend
//   will layer [nav_state] / [layout] / [bl_pane] sections on top in
//   later commits. v1 here only touches the global file.
//
// Read is fail-soft: a missing or malformed file resolves to "no
// prior state" and the chrome lands on defaults. Write is best-effort:
// failure logs but doesn't abort.

use std::path::PathBuf;

/// Snapshot of the bits we restore on launch. Anything else (cursor
/// position, scroll, focus pane, layout overrides) is in scope for
/// later B5 work but not v1.
#[derive(Debug, Clone, Default)]
pub struct GlobalState {
    /// Last mode the chrome was rendering — "files", "modules",
    /// "sessions". `None` means no prior state, land on Files default.
    pub last_mode: Option<String>,
    /// Backend session the BL pane was attached to. `None` means the
    /// default `sot-llm`.
    pub last_bl_target: Option<String>,
    /// ADR 0014 active workspace at last save. `None` resumes to the
    /// daemon's default workspace.
    pub last_workspace_id: Option<String>,
    /// ADR 0015 host registry pick. The launcher reads this on startup
    /// and routes the SSH tunnel + remote daemon spawn at the named
    /// host's entry in `hosts.toml`. `None` → launcher uses
    /// `default_host` from hosts.toml (which itself defaults to
    /// "myserver"). Persisted from `Mode::Hosts` Enter; not changed by
    /// any other path.
    pub last_host: Option<String>,
    /// Window inner size in *logical* pixels so cross-DPR launches
    /// don't blow up the geometry. `None` falls through to the
    /// hard-coded default in gpu.rs.
    pub window_w: Option<f64>,
    pub window_h: Option<f64>,
    /// Window outer position in *logical* pixels. `None` lets the OS
    /// pick a position. Saved values stick to whatever monitor /
    /// arrangement was active at save time — if the user disconnects
    /// that monitor, winit clamps back on screen automatically.
    pub window_x: Option<f64>,
    pub window_y: Option<f64>,
    /// Whether the window was in (borderless) fullscreen at last save.
    /// `Some(true)` re-enters fullscreen on the next launch so a
    /// self-relaunch (ADR 0017) doesn't drop the user out of FS. `None`
    /// / `Some(false)` → windowed, using the saved geometry above.
    pub fullscreen: Option<bool>,
    /// Runtime font-scale multiplier (`text_scale_mult`, Ctrl+=/-/0) at
    /// last save, so a self-relaunch (ADR 0017) comes back at the same
    /// zoom instead of resetting to 1.0. `None` → default 1.0.
    pub font_scale: Option<f64>,
    /// Nav cursor's selected node id at last save (e.g.
    /// `files:rust/frontend/src/gpu.rs`), restored best-effort when that
    /// row is present in the reloaded tree. `None` → land on the default
    /// cursor.
    pub nav_selected_id: Option<String>,
    /// Nav tree scroll offset (rows) at last save, paired with
    /// `nav_selected_id`.
    pub nav_scroll: Option<u16>,
}

pub fn state_path() -> PathBuf {
    let base = config_dir();
    let mut p = base.join("sot");
    // Avoid the gethostname crate in the frontend (one fewer dep on a
    // hot build path). $HOSTNAME on Linux, $COMPUTERNAME on Windows;
    // fall back to "unknown" so the file is still writable. The Linux
    // home is shared across the Linux cohort, so the
    // hostname suffix is what stops them from clobbering each other.
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    p.push(format!("state-{host}.toml"));
    p
}

fn config_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(v);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config");
        return p;
    }
    // Windows: %APPDATA% then a flat fallback.
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata);
    }
    PathBuf::from(".")
}

/// Read the global state, returning defaults on any error. Best-effort
/// — a corrupted file is treated as "no prior state" and overwritten on
/// the next save.
pub fn load() -> GlobalState {
    let path = state_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return GlobalState::default(),
    };
    let mut g = GlobalState::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = strip_quotes(value.trim());
        match key {
            "last_mode" => g.last_mode = Some(value.to_string()),
            "last_bl_target" => g.last_bl_target = Some(value.to_string()),
            "last_workspace_id" => g.last_workspace_id = Some(value.to_string()),
            "last_host" => g.last_host = Some(value.to_string()),
            "window_w" => g.window_w = value.parse().ok(),
            "window_h" => g.window_h = value.parse().ok(),
            "window_x" => g.window_x = value.parse().ok(),
            "window_y" => g.window_y = value.parse().ok(),
            "fullscreen" => g.fullscreen = value.parse().ok(),
            "font_scale" => g.font_scale = value.parse().ok(),
            "nav_selected_id" => g.nav_selected_id = Some(value.to_string()),
            "nav_scroll" => g.nav_scroll = value.parse().ok(),
            _ => {}
        }
    }
    g
}

/// Write the global state atomically (temp + rename). Failures are
/// logged warns; resume just won't work next launch.
pub fn save(state: &GlobalState) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %e, dir = ?parent, "could not create state dir");
            return;
        }
    }
    let mut body = String::new();
    body.push_str("# sot frontend resume state (B5)\n");
    if let Some(m) = &state.last_mode {
        body.push_str(&format!("last_mode = {}\n", toml_quote(m)));
    }
    if let Some(t) = &state.last_bl_target {
        body.push_str(&format!("last_bl_target = {}\n", toml_quote(t)));
    }
    if let Some(w) = &state.last_workspace_id {
        body.push_str(&format!("last_workspace_id = {}\n", toml_quote(w)));
    }
    if let Some(h) = &state.last_host {
        body.push_str(&format!("last_host = {}\n", toml_quote(h)));
    }
    if let Some(v) = state.window_w {
        body.push_str(&format!("window_w = {v}\n"));
    }
    if let Some(v) = state.window_h {
        body.push_str(&format!("window_h = {v}\n"));
    }
    if let Some(v) = state.window_x {
        body.push_str(&format!("window_x = {v}\n"));
    }
    if let Some(v) = state.window_y {
        body.push_str(&format!("window_y = {v}\n"));
    }
    if let Some(v) = state.fullscreen {
        body.push_str(&format!("fullscreen = {v}\n"));
    }
    if let Some(v) = state.font_scale {
        body.push_str(&format!("font_scale = {v}\n"));
    }
    if let Some(id) = &state.nav_selected_id {
        body.push_str(&format!("nav_selected_id = {}\n", toml_quote(id)));
    }
    if let Some(v) = state.nav_scroll {
        body.push_str(&format!("nav_scroll = {v}\n"));
    }
    let tmp = path.with_extension("toml.tmp");
    if let Err(e) = std::fs::write(&tmp, body.as_bytes()) {
        tracing::warn!(error = %e, path = ?tmp, "state write failed");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        tracing::warn!(error = %e, from = ?tmp, to = ?path, "state rename failed");
    }
}

fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests in this module mutate process-global env vars
    // (XDG_CONFIG_HOME, HOSTNAME) which would otherwise race when
    // cargo runs tests in parallel. Serialize them with a single mutex
    // taken at fixture construction.
    static SERIAL: Mutex<()> = Mutex::new(());

    struct Guard {
        _serial: std::sync::MutexGuard<'static, ()>,
        xdg: Option<std::ffi::OsString>,
        host: Option<std::ffi::OsString>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            match self.xdg.take() {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match self.host.take() {
                Some(v) => std::env::set_var("HOSTNAME", v),
                None => std::env::remove_var("HOSTNAME"),
            }
        }
    }

    fn set_test_env() -> (Guard, PathBuf) {
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "sot-state-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let g = Guard {
            _serial: serial,
            xdg: std::env::var_os("XDG_CONFIG_HOME"),
            host: std::env::var_os("HOSTNAME"),
        };
        std::env::set_var("XDG_CONFIG_HOME", &tmp);
        std::env::set_var("HOSTNAME", "test-host");
        (g, tmp)
    }

    #[test]
    fn load_returns_defaults_on_missing_file() {
        let (_g, _tmp) = set_test_env();
        let s = load();
        assert!(s.last_mode.is_none());
        assert!(s.last_bl_target.is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let (_g, tmp) = set_test_env();
        let original = GlobalState {
            last_mode: Some("sessions".to_string()),
            last_bl_target: Some("sot-be-mypkg".to_string()),
            last_workspace_id: Some("mypkg".to_string()),
            last_host: Some("myhost".to_string()),
            window_x: None,
            window_y: None,
            window_w: None,
            window_h: None,
            fullscreen: Some(true),
            font_scale: Some(1.3),
            nav_selected_id: Some("files:src/foo.jl".to_string()),
            nav_scroll: Some(7),
        };
        save(&original);
        let loaded = load();
        assert_eq!(loaded.last_mode.as_deref(), Some("sessions"));
        assert_eq!(loaded.last_bl_target.as_deref(), Some("sot-be-mypkg"));
        assert_eq!(loaded.last_host.as_deref(), Some("myhost"));
        assert_eq!(loaded.fullscreen, Some(true));
        assert_eq!(loaded.font_scale, Some(1.3));
        assert_eq!(loaded.nav_selected_id.as_deref(), Some("files:src/foo.jl"));
        assert_eq!(loaded.nav_scroll, Some(7));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn state_path_uses_hostname_suffix() {
        let (_g, tmp) = set_test_env();
        // Normalize path separator so this passes on Windows too —
        // PathBuf uses `\` there and the literal in the assertion is
        // OS-agnostic.
        let normalized = state_path().to_string_lossy().replace('\\', "/");
        assert!(
            normalized.ends_with("/sot/state-test-host.toml"),
            "got {normalized:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_tolerates_garbage_lines() {
        let (_g, tmp) = set_test_env();
        let path = state_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# comment\n[some_section]\ngarbage line without equals\nlast_mode = \"files\"\n",
        )
        .unwrap();
        let s = load();
        assert_eq!(s.last_mode.as_deref(), Some("files"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
