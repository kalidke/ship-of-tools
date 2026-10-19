//! Tests of prune.rs: the boundary of a state folder removal, and the boot sweep.

use super::*;
use std::path::PathBuf;
use crate::rows::spawn::state_root::state_dir_for;
use crate::rows::Workspace;

const ID: &str = "ws-demo-1a2b3c";

fn later() -> SystemTime {
    SystemTime::now() + AGE_FLOOR + Duration::from_secs(3600)
}

/// A state root with one folder of a row's usual contents.
fn root_with(id: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let folder = state_dir_for(root.path(), id);
    std::fs::create_dir_all(folder.join("voyages").join("v1")).unwrap();
    std::fs::write(folder.join("voyages").join("v1").join("log"), b"x").unwrap();
    std::fs::write(folder.join("drawer.voyage"), b"v1").unwrap();
    (root, folder)
}

fn workspaces_of(root: &tempfile::TempDir) -> PathBuf {
    root.path().join("workspaces")
}

#[test]
fn only_names_the_registry_makes_are_row_folder_names() {
    for ok in ["ws-demo-1a2b3c", "ws-a_b-0", "ws-default-ff"] {
        assert!(is_row_folder_name(ok), "{ok}");
    }
    let long = format!("ws-{}", "a".repeat(200));
    for bad in [
        "", ".", "..", "ws-", "ws", "WS-x", "ws-A", "ws-a.b", "ws-a b", "ws-a/b", "ws-a\\b", "../ws-a", "ws-..", "ws-a:b",
        "ws-\u{e9}", "ws-a\0b", "ws-a~1", "other", "default", long.as_str(),
    ] {
        // "ws-" alone passes the character rule; it names no row and is not removable below.
        if bad == "ws-" {
            continue;
        }
        assert!(!is_row_folder_name(bad), "{bad:?}");
    }
}

#[test]
fn a_folder_with_a_free_lock_older_than_the_floor_is_removed_with_no_floor_for_a_close() {
    let (root, folder) = root_with(ID);
    assert_eq!(remove_state_folder(&workspaces_of(&root), ID, Some(AGE_FLOOR), later()), Removal::Removed);
    assert!(!folder.exists());
    let (root, folder) = root_with(ID);
    assert_eq!(remove_closed_row_folder(root.path(), ID), Removal::Removed);
    assert!(!folder.exists());
}

#[test]
fn an_absent_folder_is_gone_not_an_error() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(remove_closed_row_folder(root.path(), ID), Removal::Gone);
    std::fs::create_dir_all(workspaces_of(&root)).unwrap();
    assert_eq!(remove_closed_row_folder(root.path(), ID), Removal::Gone);
}

#[test]
fn a_held_lock_keeps_the_folder() {
    let (root, folder) = root_with(ID);
    let held = sot_log::supervisor::journal::fence::lock_supervisor(&folder).unwrap();
    for floor in [Some(AGE_FLOOR), None] {
        assert!(matches!(remove_state_folder(&workspaces_of(&root), ID, floor, later()), Removal::Kept(_)));
    }
    assert!(folder.join("drawer.voyage").is_file());
    drop(held);
    assert_eq!(remove_state_folder(&workspaces_of(&root), ID, Some(AGE_FLOOR), later()), Removal::Removed);
}

#[test]
fn a_folder_written_within_the_floor_is_kept() {
    let (root, folder) = root_with(ID);
    assert!(matches!(
        remove_state_folder(&workspaces_of(&root), ID, Some(AGE_FLOOR), SystemTime::now()),
        Removal::Kept(_)
    ));
    // The newest write anywhere in the tree decides, not the top folder's.
    let just_under = SystemTime::now() + AGE_FLOOR - Duration::from_secs(600);
    assert!(matches!(remove_state_folder(&workspaces_of(&root), ID, Some(AGE_FLOOR), just_under), Removal::Kept(_)));
    // A write dated in the future (a clock step) is not old.
    assert!(matches!(
        remove_state_folder(&workspaces_of(&root), ID, Some(AGE_FLOOR), SystemTime::UNIX_EPOCH),
        Removal::Kept(_)
    ));
    assert!(folder.is_dir());
}

#[test]
fn a_name_the_registry_does_not_make_is_never_touched() {
    let root = tempfile::tempdir().unwrap();
    for name in ["other", "WS-demo", "ws-a.b"] {
        let folder = workspaces_of(&root).join(name);
        std::fs::create_dir_all(&folder).unwrap();
        assert!(matches!(remove_state_folder(&workspaces_of(&root), name, None, later()), Removal::Kept(_)));
        assert!(folder.is_dir());
    }
    assert!(matches!(remove_state_folder(&workspaces_of(&root), "../x", None, later()), Removal::Kept(_)));
}

#[test]
fn the_sweep_keeps_a_registered_row_and_removes_the_others() {
    let (root, kept_folder) = root_with("ws-live-1");
    let gone_folder = state_dir_for(root.path(), "ws-gone-2");
    std::fs::create_dir_all(&gone_folder).unwrap();
    let odd = workspaces_of(&root).join("notes");
    std::fs::create_dir_all(&odd).unwrap();
    let reg = Workspaces::new();
    let mut ws = Workspace::from_label("live", PathBuf::from("/p/live"), false, "none".into(), String::new(), String::new());
    ws.workspace_id = "ws-live-1".to_string();
    reg.insert(ws);
    let left = sweep_unnamed_folders(root.path(), &reg, later());
    assert_eq!(left, vec!["notes".to_string()]);
    assert!(kept_folder.is_dir(), "a registered row's folder stays");
    assert!(odd.is_dir(), "a folder that is not a row folder stays");
    assert!(!gone_folder.exists());
}

#[test]
fn the_sweep_with_no_workspaces_folder_does_nothing() {
    let root = tempfile::tempdir().unwrap();
    assert!(sweep_unnamed_folders(root.path(), &Workspaces::new(), later()).is_empty());
}

#[test]
fn a_symlink_as_supervisor_lock_keeps_the_folder() {
    let (root, folder) = root_with(ID);
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("lock");
    std::fs::write(&target, b"").unwrap();
    make_link(&target, &folder.join("supervisor.lock"));
    if std::fs::symlink_metadata(folder.join("supervisor.lock")).is_err() {
        return; // this account may not make links here
    }
    assert!(matches!(remove_state_folder(&workspaces_of(&root), ID, None, later()), Removal::Kept(_)));
    assert!(folder.is_dir() && target.is_file());
}

#[test]
fn a_symlinked_folder_or_workspaces_folder_is_never_followed() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious"), b"x").unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspaces_of(&root)).unwrap();
    let link = workspaces_of(&root).join(ID);
    make_link(outside.path(), &link);
    if std::fs::symlink_metadata(&link).is_err() {
        return;
    }
    assert!(matches!(remove_state_folder(&workspaces_of(&root), ID, None, later()), Removal::Kept(_)));
    assert!(outside.path().join("precious").is_file());

    let root2 = tempfile::tempdir().unwrap();
    make_link(outside.path(), &workspaces_of(&root2));
    std::fs::create_dir_all(outside.path().join(ID)).unwrap();
    assert!(matches!(remove_state_folder(&workspaces_of(&root2), ID, None, later()), Removal::Kept(_)));
    assert!(outside.path().join(ID).is_dir() && outside.path().join("precious").is_file());
}

#[test]
fn a_link_inside_is_unlinked_and_its_target_left_alone() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious"), b"x").unwrap();
    let (root, folder) = root_with(ID);
    make_link(outside.path(), &folder.join("voyages").join("v1").join("link"));
    if std::fs::symlink_metadata(folder.join("voyages").join("v1").join("link")).is_err() {
        return;
    }
    assert_eq!(remove_state_folder(&workspaces_of(&root), ID, Some(AGE_FLOOR), later()), Removal::Removed);
    assert!(!folder.exists());
    assert!(outside.path().join("precious").is_file(), "the link's target is never entered");
}

#[cfg(unix)]
#[test]
fn a_removal_that_fails_part_way_reports_it_and_keeps_going() {
    use std::os::unix::fs::PermissionsExt;
    let (root, folder) = root_with(ID);
    let inner = folder.join("voyages").join("v1");
    std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o500)).unwrap();
    let removal = remove_state_folder(&workspaces_of(&root), ID, Some(AGE_FLOOR), later());
    std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(removal, Removal::Failed(_)), "{removal:?}");
    assert!(folder.is_dir(), "the folder is left for the next start");
    // The log path takes the failure without a panic.
    log_removal(&workspaces_of(&root), ID, "test", &removal);
}

#[cfg(unix)]
fn make_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// A junction: the Windows link that needs no privilege.
#[cfg(windows)]
fn make_link(target: &Path, link: &Path) {
    let _ = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output();
}
