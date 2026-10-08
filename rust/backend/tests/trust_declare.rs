//! Offline declaration behavior against an isolated home, with no daemon startup.
#[path = "support/sotd.rs"]
mod sotd;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    prefix: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let config = if cfg!(windows) {
            root.join("local/sot/config")
        } else {
            root.join("config/sot")
        };
        let prefix = home.join("projects # ' quoted");
        std::fs::create_dir_all(&prefix).unwrap();
        Self {
            _temp: temp,
            root,
            config,
            prefix,
        }
    }
    fn run(&self, args: &[&std::ffi::OsStr]) -> (bool, String, String) {
        let mut cmd = sotd::sotd_command();
        cmd.args(args)
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("USERPROFILE", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("LOCALAPPDATA", self.root.join("local"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("SOT_RUNTIME_DIR", self.root.join("runtime"))
            .env("SOT_COMM_HOME", self.root.join("comm"))
            .env("SOT_NO_UPDATE", "1")
            .env_remove("CLAUDE_CONFIG_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[allow(
            clippy::disallowed_methods,
            reason = "test-only offline command; retained child is drained and bounded"
        )]
        let child = cmd.spawn().unwrap();
        let (status, out, err) =
            sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(20));
        (status.success(), out, err)
    }
    fn declare(&self) -> (bool, String, String) {
        self.run(&[
            "trust".as_ref(),
            "declare".as_ref(),
            self.prefix.as_os_str(),
        ])
    }
    fn settings(&self) -> PathBuf {
        self.config.join("settings.toml")
    }
    fn no_daemon_output(&self) {
        for name in ["state", "runtime", "comm"] {
            assert!(
                !self.root.join(name).exists(),
                "W1 offline command created daemon output"
            );
        }
    }
}
#[test]
fn offline_trust_declare_preserves_settings() {
    let f = Fixture::new();
    std::fs::create_dir_all(&f.config).unwrap();
    let before = b"# preserved settings\n[layout]\npreset = 'auto'\n";
    std::fs::write(f.settings(), before).unwrap();
    let (ok, out, _) = f.declare();
    assert!(
        ok && out.trim() == "Declared",
        "W1 offline trust declare did not return Declared"
    );
    let after = std::fs::read(f.settings()).unwrap();
    assert!(after.starts_with(before));
    let (ok, out, _) = f.declare();
    assert!(ok && out.trim() == "Kept");
    assert_eq!(std::fs::read(f.settings()).unwrap(), after);
    f.no_daemon_output();
    println!("W1 C1 offline declaration PASS");
}

#[test]
fn offline_cli_matrix_creates_keeps_and_rejects_without_startup() {
    let f = Fixture::new();
    let (ok, out, _) = f.declare();
    assert!(ok && out.trim() == "Declared");
    let text = std::fs::read_to_string(f.settings()).unwrap();
    let doc: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(
        doc["trust"]["root_prefix"].as_str().unwrap(),
        f.prefix.to_str().unwrap()
    );
    assert!(!text.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
    for bytes in [
        &b"[trust] # keep\nroot_prefix = ''\n"[..],
        &b"trust = { root_prefix = '/kept' }\n"[..],
        &b"trust.root_prefix = '/kept'\n"[..],
        &b"[ trust ]\n"[..],
    ] {
        std::fs::write(f.settings(), bytes).unwrap();
        let (ok, out, _) = f.declare();
        assert!(ok && out.trim() == "Kept");
        assert_eq!(std::fs::read(f.settings()).unwrap(), bytes);
    }
    for bytes in [&b"[layout"[..], &b"\xff\xfe\x00\x00"[..]] {
        std::fs::write(f.settings(), bytes).unwrap();
        let (ok, out, err) = f.declare();
        assert!(!ok && out.is_empty() && err.contains("settings.toml"));
        assert_eq!(std::fs::read(f.settings()).unwrap(), bytes);
    }
    f.no_daemon_output();
    let fresh = Fixture::new();
    for args in [
        vec!["trust".as_ref(), "declare".as_ref(), "relative".as_ref()],
        vec!["trust".as_ref(), "declare".as_ref()],
        vec!["trust".as_ref(), "wrong".as_ref(), fresh.prefix.as_os_str()],
        vec![
            "trust".as_ref(),
            "declare".as_ref(),
            fresh.prefix.as_os_str(),
            "extra".as_ref(),
        ],
    ] {
        assert!(!fresh.run(&args).0);
        assert!(!fresh.config.exists());
    }
    // Built as text: `Path::join` folds `..` away on a verbatim (`\\?\`) path, which would declare a clean prefix.
    let sep = std::path::MAIN_SEPARATOR;
    let parent_components = PathBuf::from(format!("{}{sep}..{sep}escape", fresh.prefix.display()));
    let filesystem_root: PathBuf = fresh
        .prefix
        .components()
        .take_while(|c| !matches!(c, std::path::Component::Normal(_)))
        .collect();
    for prefix in [&parent_components, &filesystem_root] {
        assert!(
            !fresh
                .run(&["trust".as_ref(), "declare".as_ref(), prefix.as_os_str()])
                .0
        );
        assert!(!fresh.config.exists());
    }
    let (ok, out, _) = fresh.run(&["trust".as_ref(), "declare".as_ref(), "--help".as_ref()]);
    assert!(ok && out.contains("Usage: sotd trust declare <absolute-prefix>"));
    assert!(!fresh.config.exists());
    fresh.no_daemon_output();
    println!("W1 C1 CLI matrix PASS: parsed settings; preserved answers; encoding and argv refused; help inert");
}
