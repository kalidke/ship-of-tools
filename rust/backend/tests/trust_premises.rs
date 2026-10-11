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

#[cfg(unix)]
#[test]
fn unix_installer_preserves_table_forms_and_reports_owner_failures() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let prefix = root.join("scope # ' \u{03bc}");
    std::fs::create_dir_all(&prefix).unwrap();
    let config = root.join("config/sot");
    std::fs::create_dir_all(&config).unwrap();
    let file = config.join("settings.toml");
    let installer =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh");
    let invoke = |binary: &std::path::Path| {
        let mut command = std::process::Command::new("/bin/bash");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("SOT_") {
                command.env_remove(key);
            }
        }
        command
            .args([
                "-c",
                "SOT_INSTALL_SOURCE_ONLY=1 . \"$1\"; installer_declare_trust \"$2\" \"$3\"",
                "w1-fixture",
            ])
            .arg(&installer)
            .arg(binary)
            .arg(&prefix)
            .env("HOME", &prefix)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[allow(
            clippy::disallowed_methods,
            reason = "test-only offline installer caller; retained child is drained within its bound"
        )]
        let child = command.spawn().unwrap();
        sot_log::test_isolated::drain(child).wait_within(std::time::Duration::from_secs(20))
    };
    let initial = "# retained\n[layout]\npreset = 'auto'\n";
    std::fs::write(&file, initial).unwrap();
    let (status, output, _) = invoke(&sotd::sotd_program());
    assert!(status.success() && output.contains("folder trust declared"));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with(initial));
    let doc: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(
        doc["trust"]["root_prefix"].as_str().unwrap(),
        prefix.to_str().unwrap()
    );
    for text in [
        "[trust] # kept\n",
        "trust = { root_prefix = '/kept' }\n",
        "trust.root_prefix = '/kept'\n",
        "[ trust ]\n",
    ] {
        std::fs::write(&file, text).unwrap();
        let (status, output, _) = invoke(&sotd::sotd_program());
        let emitted = std::fs::read_to_string(&file).unwrap();
        assert!(
            toml::from_str::<toml::Table>(&emitted).is_ok(),
            "W1 C4 Unix emitted invalid duplicate-table TOML"
        );
        assert_eq!(
            std::fs::read(&file).unwrap(),
            text.as_bytes(),
            "W1 C4 Unix existing trust answer changed"
        );
        assert!(status.success() && output.contains("folder trust kept"));
    }
    for bytes in [&b"[layout"[..], &b"\xff\xfe[\x00l\x00"[..]] {
        std::fs::write(&file, bytes).unwrap();
        let (status, output, _) = invoke(&sotd::sotd_program());
        assert!(status.success() && output.contains("folder trust not declared"));
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
    }
    let before = std::fs::read(&file).unwrap();
    let (status, output, _) = invoke(&root.join("missing-sotd"));
    assert!(status.success() && output.contains("folder trust not declared"));
    assert_eq!(std::fs::read(&file).unwrap(), before);
    let old = root.join("older-sotd");
    sot_log::test_exec::write_executable(
        &old,
        b"#!/bin/sh\nprintf 'unknown subcommand trust\\n' >&2\nexit 64\n",
    );
    let before = std::fs::read(&file).unwrap();
    let (status, output, _) = invoke(&old);
    assert!(status.success() && output.contains("folder trust not declared (exit 64)"));
    assert_eq!(std::fs::read(&file).unwrap(), before);
    println!("W1 C4 Unix matrix PASS: real caller; preserved TOML forms and invalid encodings; older binary warning; no fallback");
}
