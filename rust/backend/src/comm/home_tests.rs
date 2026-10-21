use super::*;

#[test]
fn a_new_comm_folder_is_whole_and_an_existing_registry_is_never_replaced() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("comm");
    assert!(!is_whole(&home));
    ensure(&home);
    assert!(is_whole(&home));
    assert_eq!(std::fs::read_to_string(home.join("registry.json")).unwrap(), "{\"protocol_version\": 1, \"agents\": {}}\n");
    std::fs::write(home.join("registry.json"), "not json").unwrap();
    ensure(&home);
    assert_eq!(std::fs::read_to_string(home.join("registry.json")).unwrap(), "not json", "an unreadable registry is never repaired");
    let left: Vec<_> = std::fs::read_dir(&home).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("registry.json.new")).collect();
    assert!(left.is_empty(), "the skeleton's temp file goes");
}

#[cfg(unix)]
#[test]
fn an_open_folder_an_older_release_left_is_closed_but_never_a_checkout() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("comm");
    for sub in ["inbox", "read", "self", "state"] {
        std::fs::create_dir_all(home.join(sub)).unwrap();
    }
    let files = ["registry.json", "inbox/a.jsonl", "inbox/a.lock", "read/a.cursor", "self/h__x.txt", "state/t.tick", ".registry.lock.reclaim.x"];
    for f in files {
        std::fs::write(home.join(f), "x").unwrap();
    }
    std::fs::write(home.join("mine.txt"), "x").unwrap();
    for p in std::fs::read_dir(&home).unwrap().flatten().map(|e| e.path()).chain(files.iter().map(|f| home.join(f))) {
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(if p.is_dir() { 0o755 } else { 0o644 })).unwrap();
    }
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(tighten(&home).is_empty());
    assert_eq!(mode(&home), 0o700);
    for f in files {
        assert_eq!(mode(&home.join(f)), 0o600, "{f}");
    }
    assert_eq!(mode(&home.join("mine.txt")), 0o644, "a file outside the layout keeps its mode");
    let repo = d.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(tighten(&repo)[0].contains("a git checkout"));
    assert_eq!(mode(&repo), 0o755);
}
