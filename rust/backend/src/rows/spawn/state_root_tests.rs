use super::*;

/// A full volume qualifies only a row that has run; any other preflight
/// failure stays a refusal, and a row has run once its fence file exists.
#[test]
fn a_full_volume_qualifies_only_a_row_that_has_run() {
    #[cfg(unix)]
    let full = || std::io::Error::from_raw_os_error(libc::ENOSPC);
    #[cfg(windows)]
    let full = || std::io::Error::from_raw_os_error(112);
    assert_eq!(preflight_verdict(Err(sot_log::Error::Io(full())), true), Ok(()));
    let refused = preflight_verdict(Err(sot_log::Error::Io(full())), false).unwrap_err();
    assert!(refused.contains("volume is full"), "{refused}");
    assert_eq!(preflight_verdict(Ok(()), false), Ok(()));
    let unsupported = || std::io::Error::new(std::io::ErrorKind::Unsupported, "incompatible volume");
    assert!(preflight_verdict(Err(sot_log::Error::Io(unsupported())), true).is_err());

    let dir = tempfile::tempdir().unwrap();
    assert!(!row_has_run(dir.path()));
    std::fs::write(sot_log::supervisor::journal::fence::supervisor_lock_path(dir.path()), b"").unwrap();
    assert!(row_has_run(dir.path()));
}

#[test]
fn state_dir_joins_workspaces_and_the_id() {
    let root = Path::new("/tmp/sot-state-root");
    assert_eq!(
        state_dir_for(root, "ws-alpha-1a2b"),
        PathBuf::from("/tmp/sot-state-root/workspaces/ws-alpha-1a2b")
    );
}

// Field defect: a scratch daemon watched a project root that
// CONTAINED its own state root, so the daemon's file watcher held
// directory handles inside it; on Windows an open directory handle
// blocks a rename, so `voyages\<uuid>` publication failed with a
// sharing violation and the supervisor exited terminal 69. These
// cover the predicate that now refuses that layout up front, at
// capsule create and again right before every spawn.
#[test]
fn state_root_inside_project_true_when_a_descendant() {
    let dir = tempfile::tempdir().unwrap();
    let project_root = dir.path().join("project");
    let state_root = project_root.join("state").join("workspaces").join("ws-1");
    std::fs::create_dir_all(&state_root).unwrap();
    assert!(state_root_inside_project(&state_root, &project_root));
}

#[test]
fn state_root_inside_project_true_when_equal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("shared-root");
    std::fs::create_dir_all(&root).unwrap();
    assert!(state_root_inside_project(&root, &root));
}

#[test]
fn state_root_inside_project_false_for_a_same_prefix_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let project_root = dir.path().join("project");
    let state_root = dir.path().join("project-state"); // same prefix, NOT nested
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::create_dir_all(&state_root).unwrap();
    assert!(!state_root_inside_project(&state_root, &project_root));
}

#[test]
#[cfg(unix)]
fn state_root_inside_project_resolves_a_symlinked_project_root() {
    let dir = tempfile::tempdir().unwrap();
    let real_project = dir.path().join("real-project");
    let state_root = real_project.join("state");
    std::fs::create_dir_all(&state_root).unwrap();
    let link = dir.path().join("project-link");
    std::os::unix::fs::symlink(&real_project, &link).unwrap();
    // The caller passes the SYMLINK as the project root; the state
    // root is a real descendant of what it resolves to.
    assert!(state_root_inside_project(&state_root, &link));
}
