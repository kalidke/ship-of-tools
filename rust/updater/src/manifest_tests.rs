use super::*;

fn identity() -> ReleaseIdentity {
    ReleaseIdentity {
        repo: "kalidke/ship-of-tools".into(),
        tag: "v0.6.0".into(),
        version: "0.6.0".into(),
        target: "linux-x86_64".into(),
        asset: "sot-0.6.0-linux-x86_64.tar.gz".into(),
        asset_sha256: "ab".repeat(32),
    }
}

#[tokio::test]
async fn ready_manifest_roundtrip_and_match() {
    let dir = std::env::temp_dir().join(format!("sot-updater-manifest-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir).await.unwrap();

    assert!(ReadyManifest::read(&dir).await.is_err()); // absent
    let m = ReadyManifest::new(identity(), Some("abc123".into()));
    m.write(&dir).await.unwrap();
    let back = ReadyManifest::read(&dir).await.unwrap();
    assert_eq!(back.identity, identity());
    assert!(ReadyManifest::matches(&dir, &identity()).await);

    let mut other = identity();
    other.tag = "v0.7.0".into();
    other.version = "0.7.0".into();
    other.asset = "sot-0.7.0-linux-x86_64.tar.gz".into();
    assert!(!ReadyManifest::matches(&dir, &other).await);

    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

#[test]
fn ready_manifest_parses_without_source_commit() {
    // Pre-commit-binding stages (legacy) must keep parsing.
    let text = r#"{
            "schema": 1,
            "repo": "kalidke/ship-of-tools",
            "tag": "v0.6.0",
            "version": "0.6.0",
            "target": "linux-x86_64",
            "asset": "sot-0.6.0-linux-x86_64.tar.gz",
            "asset_sha256": "abababababababababababababababababababababababababababababababab",
            "staged_at": 5
        }"#;
    let m: ReadyManifest = serde_json::from_str(text).unwrap();
    assert!(m.source_commit.is_none());
}

#[test]
fn install_manifest_parses_with_unknown_fields() {
    // `role` (schema 1's own dropped field) is unknown to this struct
    // now, same as any future key a newer installer adds; `hub`,
    // `daemon` and `frontend` are real fields this fixture also checks.
    let text = r#"{
            "schema": 1,
            "role": "local",
            "hub": "",
            "prefix": "/home/u/.local/share/sot",
            "config": "/home/u/.config/sot",
            "service": "systemd",
            "version": "0.6.0",
            "tag": "v0.6.0",
            "commit": "abc123",
            "daemon": true,
            "frontend": false,
            "future_field": {"x": 1}
        }"#;
    let m: InstallManifest = serde_json::from_str(text).unwrap();
    assert_eq!(m.updates_root(), PathBuf::from("/home/u/.local/share/sot/updates"));
    assert_eq!(m.daemon, Some(true));
    assert_eq!(m.frontend, Some(false));
}

// The three states of install.json, and the one sentence for the middle one.
#[test]
fn load_tells_absent_from_unreadable_from_read() {
    let dir = std::env::temp_dir().join(format!("sot-updater-load-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(INSTALL_MANIFEST);
    assert!(matches!(InstallManifest::load(&file), InstallFile::Absent));
    std::fs::write(&file, r#"{"schema":1,"prefix":"/p","version":"0.6.7","pinned":tr"#).unwrap();
    let InstallFile::Unreadable(cut) = InstallManifest::load(&file) else { panic!("a cut file is unreadable") };
    assert_eq!(cut.path, file);
    assert_eq!(cut.updates_root(), dir.join("updates"));
    let said = cut.sentence();
    assert!(said.starts_with(UNREADABLE_LEAD), "{said}");
    assert!(said.contains(&file.display().to_string()) && said.contains(&cut.cause), "names the file and the cause: {said}");
    let run = if cfg!(windows) { "install.ps1 -Version" } else { "install.sh --version" };
    assert!(said.contains(run), "names the remedy that works on this OS: {said}");
    std::fs::write(&file, "\u{feff}{\"schema\":1,\"prefix\":\"/p\"}").unwrap();
    assert!(matches!(InstallManifest::load(&file), InstallFile::Read(_)), "a BOM alone is not unreadable");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn install_manifest_parses_without_a_role_field() {
    // The regression this pins: scripts/install.sh (plan step 6) no
    // longer writes `role` at all — a manifest missing the key
    // entirely, not just an empty one, must still parse. `daemon` and
    // `frontend` are also absent here (a pre-existing manifest from
    // before those fields shipped) and must default to `None`, not a
    // parse error.
    let text = r#"{
            "schema": 1,
            "prefix": "/home/u/.local/share/sot",
            "hub": "host-2"
        }"#;
    let m: InstallManifest = serde_json::from_str(text).unwrap();
    assert_eq!(m.prefix, PathBuf::from("/home/u/.local/share/sot"));
    assert_eq!(m.daemon, None);
    assert_eq!(m.frontend, None);
}
