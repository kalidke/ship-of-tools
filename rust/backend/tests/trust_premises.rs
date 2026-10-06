//! W1 preparation witnesses. Real-Claude consumption and interactive recognition are human proof limits.
#[path = "support/sotd.rs"]
mod sotd;
use std::process::Stdio;
use std::time::Duration;

#[test]
fn preparation_declaration_spelling_survives_the_real_cli() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let prefix = root.join("scope with # and quotes '");
    std::fs::create_dir(&prefix).unwrap();
    let mut command = sotd::sotd_command();
    command
        .args(["trust", "declare"])
        .arg(&prefix)
        .current_dir(&root)
        .env("HOME", &root)
        .env("USERPROFILE", &root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("LOCALAPPDATA", root.join("local"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("SOT_RUNTIME_DIR", root.join("runtime"))
        .env("SOT_COMM_HOME", root.join("comm"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[allow(
        clippy::disallowed_methods,
        reason = "test-only offline owner witness; retained child is bounded and drained"
    )]
    let child = command.spawn().unwrap();
    let (status, out, _) =
        sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(20));
    assert!(status.success() && out.trim() == "Declared");
    let config = if cfg!(windows) {
        root.join("local/sot/config")
    } else {
        root.join("config/sot")
    };
    let text = std::fs::read_to_string(config.join("settings.toml")).unwrap();
    let doc: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(
        doc["trust"]["root_prefix"].as_str().unwrap(),
        prefix.to_str().unwrap()
    );
    for name in ["state", "runtime", "comm"] {
        assert!(!root.join(name).exists());
    }
    println!("W1 declaration preparation PASS: exact prefix spelling in selected settings; no daemon output");
    println!("W1 P0/P1/P5 PROOF LIMIT: real-Claude config consumption, observed child key and config semantics require the human release done test");
}
