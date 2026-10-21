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
    tmp: PathBuf,
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
        // Removed on pass and on failure, so no test leaves its folder in TMPDIR.
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn set_test_env() -> Guard {
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
        tmp,
    };
    std::env::set_var("XDG_CONFIG_HOME", &g.tmp);
    std::env::set_var("HOSTNAME", "test-host");
    g
}

#[test]
fn load_returns_defaults_on_missing_file() {
    let _g = set_test_env();
    let s = load();
    assert!(s.last_mode.is_none());
    assert!(s.last_bl_target.is_none());
}

#[test]
fn save_then_load_round_trips() {
    let _g = set_test_env();
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
}

#[test]
fn state_path_uses_hostname_suffix() {
    let _g = set_test_env();
    // Normalize path separator so this passes on Windows too —
    // PathBuf uses `\` there and the literal in the assertion is
    // OS-agnostic.
    let normalized = state_path().to_string_lossy().replace('\\', "/");
    assert!(
        normalized.ends_with("/sot/state-test-host.toml"),
        "got {normalized:?}"
    );
}

#[test]
fn load_tolerates_garbage_lines() {
    let _g = set_test_env();
    let path = state_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "# comment\n[some_section]\ngarbage line without equals\nlast_mode = \"files\"\n",
    )
    .unwrap();
    let s = load();
    assert_eq!(s.last_mode.as_deref(), Some("files"));
}
