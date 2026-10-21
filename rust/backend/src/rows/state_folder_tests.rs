//! Tests of state_folder.rs: the boundary of a removal, the close's tombstone, and the start sweep with its whole-load rule.

use super::*;
use crate::rows::spawn::state_root::state_dir_for;
use crate::rows::Workspace;
use std::path::PathBuf;

const ID: &str = "ws-demo-1a2b3c";
const VOYAGE: &str = "a1b2c3d4-e5f6-4890-9abc-def012345678";

fn later() -> SystemTime {
    SystemTime::now() + AGE_FLOOR + Duration::from_secs(3600)
}

/// A state root with one folder of a row's usual contents.
fn root_with(id: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let folder = state_dir_for(root.path(), id);
    let voyage = sot_log::supervisor::voyage_root_path(&folder, VOYAGE);
    std::fs::create_dir_all(&voyage).unwrap();
    std::fs::write(voyage.join(sot_log::store::voyage::WRITER_LOCK), b"").unwrap();
    std::fs::write(voyage.join("log"), b"x").unwrap();
    std::fs::write(folder.join("drawer.voyage"), VOYAGE).unwrap();
    (root, folder)
}

fn workspaces_of(root: &tempfile::TempDir) -> PathBuf {
    root.path().join("workspaces")
}

fn tombstone_of(root: &tempfile::TempDir, id: &str) -> PathBuf {
    workspaces_of(root).join(format!("{TOMBSTONE_PREFIX}{id}"))
}

fn none_named() -> Result<HashSet<String>, String> {
    Ok(HashSet::new())
}

/// Retire then finish, as the sweep does.
fn remove_now(workspaces_dir: &Path, id: &str, floor: Option<Duration>) -> Removal {
    match retire(workspaces_dir, id, floor, later()) {
        Ok(tombstone) => finish_tombstone(workspaces_dir, &tombstone),
        Err(removal) => removal,
    }
}

fn wait_gone(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while path.exists() {
        assert!(std::time::Instant::now() < deadline, "{path:?} was not removed");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn only_names_the_registry_makes_are_row_folder_names() {
    for ok in ["ws-demo-1a2b3c", "ws-a_b-0", "ws-default-ff"] {
        assert!(is_row_folder_name(ok), "{ok}");
    }
    let long = format!("ws-{}", "a".repeat(200));
    for bad in [
        "", ".", "..", "ws", "WS-x", "ws-A", "ws-a.b", "ws-a b", "ws-a/b", "ws-a\\b", "../ws-a", "ws-..", "ws-a:b",
        "ws-\u{e9}", "ws-a\0b", "ws-a~1", "other", "default", ".closed-ws-a", long.as_str(),
    ] {
        assert!(!is_row_folder_name(bad), "{bad:?}");
    }
    assert!(is_tombstone_name(".closed-ws-demo-1"));
    for bad in [".closed-", ".closed-other", ".closed-../ws-a", "closed-ws-a", ".closed-.closed-ws-a", ".closed-ws-a/b"] {
        assert!(!is_tombstone_name(bad), "{bad:?}");
    }
}

#[test]
fn a_close_renames_the_folder_and_a_thread_removes_the_tombstone() {
    let (root, folder) = root_with(ID);
    close_row_folder(root.path(), ID);
    assert!(!folder.exists(), "the row's own folder is gone at once");
    wait_gone(&tombstone_of(&root, ID));
    assert!(workspaces_of(&root).is_dir());
}

#[test]
fn a_close_of_an_absent_folder_does_nothing() {
    let root = tempfile::tempdir().unwrap();
    close_row_folder(root.path(), ID);
    std::fs::create_dir_all(workspaces_of(&root)).unwrap();
    close_row_folder(root.path(), ID);
    assert_eq!(remove_now(&workspaces_of(&root), ID, None), Removal::Gone);
}

#[test]
fn a_close_that_keeps_the_folder_logs_why_at_info() {
    let (root, folder) = root_with(ID);
    let held = sot_log::supervisor::journal::fence::lock_supervisor(&folder).unwrap();
    let log = sot_log::test_log::capture();
    close_row_folder(root.path(), ID);
    let text = log.text();
    assert!(text.contains(" INFO ") && text.contains("was kept") && text.contains("supervisor.lock is held"), "{text}");
    assert!(folder.is_dir());
    drop(held);
}

#[test]
fn a_held_lock_keeps_the_folder() {
    let (root, folder) = root_with(ID);
    let held = sot_log::supervisor::journal::fence::lock_supervisor(&folder).unwrap();
    for floor in [Some(AGE_FLOOR), None] {
        assert!(matches!(remove_now(&workspaces_of(&root), ID, floor), Removal::Kept(_)));
    }
    assert!(folder.join("drawer.voyage").is_file());
    drop(held);
    assert_eq!(remove_now(&workspaces_of(&root), ID, Some(AGE_FLOOR)), Removal::Removed);
}

#[test]
fn a_held_voyage_writer_lock_or_a_scopes_record_keeps_the_folder() {
    let (root, folder) = root_with(ID);
    let voyage = sot_log::supervisor::voyage_root_path(&folder, VOYAGE);
    let writer = voyage.join(sot_log::store::voyage::WRITER_LOCK);
    std::fs::write(&writer, b"").unwrap();
    let leg = sot_platform::host::lock_writer(&writer).unwrap();
    assert_eq!(
        remove_now(&workspaces_of(&root), ID, Some(AGE_FLOOR)),
        Removal::Kept("a leg holds the voyage's writer.lock")
    );
    drop(leg);
    #[cfg(target_os = "linux")]
    {
        std::fs::write(folder.join("row-scopes"), b"user.slice/x.scope\n").unwrap();
        assert!(matches!(remove_now(&workspaces_of(&root), ID, Some(AGE_FLOOR)), Removal::Kept(r) if r.contains("row-scopes")));
        assert!(folder.is_dir());
        std::fs::remove_file(folder.join("row-scopes")).unwrap();
    }
    assert_eq!(remove_now(&workspaces_of(&root), ID, Some(AGE_FLOOR)), Removal::Removed);
}

#[test]
fn a_row_that_never_started_is_removed_and_one_with_voyages_but_no_pointer_is_kept() {
    let root = tempfile::tempdir().unwrap();
    let folder = state_dir_for(root.path(), ID);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("supervisor.lock"), b"").unwrap();
    assert_eq!(remove_now(&workspaces_of(&root), ID, Some(AGE_FLOOR)), Removal::Removed);
    let folder = state_dir_for(root.path(), ID);
    std::fs::create_dir_all(folder.join("voyages").join("v1")).unwrap();
    assert!(matches!(remove_now(&workspaces_of(&root), ID, Some(AGE_FLOOR)), Removal::Kept(_)));
    assert!(folder.is_dir());
}

#[test]
fn a_folder_written_within_the_floor_is_kept() {
    let (root, folder) = root_with(ID);
    let wd = workspaces_of(&root);
    assert!(matches!(retire(&wd, ID, Some(AGE_FLOOR), SystemTime::now()), Err(Removal::Kept(_))));
    // The newest write anywhere in the tree decides, not the top folder's.
    let just_under = SystemTime::now() + AGE_FLOOR - Duration::from_secs(600);
    assert!(matches!(retire(&wd, ID, Some(AGE_FLOOR), just_under), Err(Removal::Kept(_))));
    // A write dated in the future (a clock step) is not old.
    assert!(matches!(retire(&wd, ID, Some(AGE_FLOOR), SystemTime::UNIX_EPOCH), Err(Removal::Kept(_))));
    assert!(folder.is_dir());
}

#[test]
fn a_name_the_registry_does_not_make_is_never_touched() {
    let root = tempfile::tempdir().unwrap();
    for name in ["other", "WS-demo", "ws-a.b"] {
        let folder = workspaces_of(&root).join(name);
        std::fs::create_dir_all(&folder).unwrap();
        assert!(matches!(remove_now(&workspaces_of(&root), name, None), Removal::Kept(_)));
        assert!(folder.is_dir());
    }
    assert!(matches!(remove_now(&workspaces_of(&root), "../x", None), Removal::Kept(_)));
    assert!(matches!(finish_tombstone(&workspaces_of(&root), "other"), Removal::Kept(_)));
}

#[test]
fn the_sweep_removes_tombstones_without_a_registry_and_keeps_a_registered_row() {
    let (root, kept_folder) = root_with("ws-live-1");
    let tomb = tombstone_of(&root, "ws-closed-2");
    std::fs::create_dir_all(tomb.join("voyages")).unwrap();
    let reg = Workspaces::new();
    let mut ws = Workspace::from_label("live", PathBuf::from("/p/live"), false, "none".into(), String::new(), String::new());
    ws.workspace_id = "ws-live-1".to_string();
    reg.insert(ws);
    let unreadable: Result<HashSet<String>, String> = Err("a registry did not read".into());
    let log = sot_log::test_log::capture();
    let left = sweep_with(root.path(), &reg, &unreadable, later());
    assert!(left.is_empty(), "{left:?}");
    assert!(!tomb.exists(), "a tombstone goes with no registry read");
    assert!(kept_folder.is_dir());
    let text = log.text();
    assert!(text.contains("a registry did not read") && text.contains("no folder is removed as unnamed"), "{text}");
    assert!(text.contains("removed a row's state folder") && text.contains("/resume") && text.contains("terminal recording"), "{text}");
}

#[test]
fn the_sweep_removes_an_unnamed_folder_and_keeps_one_another_registry_names() {
    let (root, gone) = root_with("ws-gone-2");
    let named_elsewhere = state_dir_for(root.path(), "ws-elsewhere-3");
    std::fs::create_dir_all(&named_elsewhere).unwrap();
    let odd = workspaces_of(&root).join("notes");
    std::fs::create_dir_all(&odd).unwrap();
    let named = Ok(HashSet::from(["ws-elsewhere-3".to_string()]));
    let left = sweep_with(root.path(), &Workspaces::new(), &named, later());
    assert!(left.is_empty(), "{left:?}");
    assert!(!gone.exists());
    assert!(named_elsewhere.is_dir(), "a row some registry names keeps its folder");
    assert!(odd.is_dir(), "a folder that is not a row folder stays");
    // Unnamed but written within the floor: left, and listed.
    assert_eq!(sweep_with(root.path(), &Workspaces::new(), &none_named(), SystemTime::now()), vec!["ws-elsewhere-3".to_string()]);
}

#[test]
fn the_sweep_with_no_workspaces_folder_does_nothing() {
    let root = tempfile::tempdir().unwrap();
    assert!(sweep_with(root.path(), &Workspaces::new(), &none_named(), later()).is_empty());
}

#[test]
fn a_failure_in_one_folder_logs_and_the_sweep_goes_on() {
    let root = tempfile::tempdir().unwrap();
    let first = tombstone_of(&root, "ws-aaa-1");
    std::fs::create_dir_all(first.join("voyages")).unwrap();
    std::fs::write(first.join("voyages").join("log"), b"x").unwrap();
    let second = tombstone_of(&root, "ws-zzz-2");
    std::fs::create_dir_all(&second).unwrap();
    let _blocked = block_removal(&first.join("voyages"));
    let log = sot_log::test_log::capture();
    let left = sweep_with(root.path(), &Workspaces::new(), &none_named(), later());
    let text = log.text();
    assert_eq!(left, vec![".closed-ws-aaa-1".to_string()]);
    assert!(text.contains(" WARN ") && text.contains("failed part way") && text.contains(".closed-ws-aaa-1"), "{text}");
    assert!(first.is_dir(), "the failed one stays for the next start");
    assert!(!second.exists(), "the sweep went on to the next folder");
}

/// Make removing the file in `dir` fail: Unix takes the folder's write bit away; Windows holds the file open.
#[cfg(unix)]
fn block_removal(dir: &Path) -> Box<dyn std::any::Any> {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    Box::new(Restore(dir.to_path_buf()))
}

#[cfg(windows)]
fn block_removal(dir: &Path) -> Box<dyn std::any::Any> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ only: no delete sharing, so removing the file fails with a sharing violation.
    const FILE_SHARE_READ: u32 = 1;
    let held = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(dir.join("log"));
    Box::new(held.expect("the file to hold open"))
}

#[test]
fn a_symlink_as_supervisor_lock_keeps_the_folder() {
    let (root, folder) = root_with(ID);
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("lock");
    std::fs::write(&target, b"").unwrap();
    make_link(&target, &folder.join("supervisor.lock"));
    assert!(matches!(remove_now(&workspaces_of(&root), ID, None), Removal::Kept(_)));
    assert!(folder.is_dir() && target.is_file());
}

#[test]
fn a_symlinked_folder_or_workspaces_folder_is_never_followed() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious"), b"x").unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspaces_of(&root)).unwrap();
    make_link(outside.path(), &workspaces_of(&root).join(ID));
    assert!(matches!(remove_now(&workspaces_of(&root), ID, None), Removal::Kept(_)));
    // A tombstone that is a link is not followed either.
    make_link(outside.path(), &tombstone_of(&root, "ws-linked-9"));
    assert!(matches!(finish_tombstone(&workspaces_of(&root), ".closed-ws-linked-9"), Removal::Kept(_)));
    assert!(outside.path().join("precious").is_file());

    let root2 = tempfile::tempdir().unwrap();
    make_link(outside.path(), &workspaces_of(&root2));
    std::fs::create_dir_all(outside.path().join(ID)).unwrap();
    assert!(matches!(remove_now(&workspaces_of(&root2), ID, None), Removal::Kept(_)));
    assert!(outside.path().join(ID).is_dir() && outside.path().join("precious").is_file());
}

#[test]
fn a_link_inside_is_unlinked_and_its_target_left_alone() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious"), b"x").unwrap();
    let (root, folder) = root_with(ID);
    make_link(outside.path(), &sot_log::supervisor::voyage_root_path(&folder, VOYAGE).join("link"));
    assert_eq!(remove_now(&workspaces_of(&root), ID, Some(AGE_FLOOR)), Removal::Removed);
    assert!(!folder.exists());
    assert!(outside.path().join("precious").is_file(), "the link's target is never entered");
}

#[cfg(unix)]
fn make_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// A junction: the Windows link that needs no privilege. A link that cannot be made is a setup failure, never a pass.
#[cfg(windows)]
fn make_link(target: &Path, link: &Path) {
    let mut mklink = std::process::Command::new("cmd");
    mklink.args(["/C", "mklink", "/J"]).arg(link).arg(target);
    sot_platform::test_wait::run_within(&mut mklink, Duration::from_secs(30)).ok();
    assert!(std::fs::symlink_metadata(link).is_ok(), "mklink /J failed: no link at {link:?}");
}

// The whole-load rule against registries on disk, in a scratch config and state root.

struct Env {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Env {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn pinned(tmp: &Path, host: &str) -> Env {
    let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = ["XDG_CONFIG_HOME", "XDG_STATE_HOME", "LOCALAPPDATA", "SOT_SELF_HOST", "SOT_COMM_HOME"]
        .iter()
        .map(|k| (*k, std::env::var_os(k)))
        .collect();
    std::env::set_var("XDG_CONFIG_HOME", tmp.join("config"));
    std::env::set_var("XDG_STATE_HOME", tmp.join("state"));
    std::env::set_var("LOCALAPPDATA", tmp.join("local"));
    std::env::set_var("SOT_COMM_HOME", tmp.join("comm"));
    std::env::set_var("SOT_SELF_HOST", host);
    Env { saved, _serial: serial }
}

/// A row registered on disk under the pinned host, with an idle state folder.
fn registered_row() -> (String, PathBuf, PathBuf) {
    let mut ws = Workspace::from_label("proj", PathBuf::from("/p/proj"), false, "none".into(), String::new(), String::new());
    ws.runtime = "capsule".into();
    let id = ws.workspace_id.clone();
    let toml = crate::rows::store::save(&ws).unwrap();
    let state_root = sot_platform::host::state_dir::sot_state_dir().unwrap();
    let folder = state_dir_for(&state_root, &id);
    let voyage = sot_log::supervisor::voyage_root_path(&folder, VOYAGE);
    std::fs::create_dir_all(&voyage).unwrap();
    std::fs::write(voyage.join(sot_log::store::voyage::WRITER_LOCK), b"").unwrap();
    std::fs::write(folder.join("drawer.voyage"), VOYAGE).unwrap();
    (id, folder, toml)
}

/// A start, in the daemon's order: the registry scan, this host's default-row save (`seed_default_row`), then the sweep a
/// day on.
fn start(workspaces: &Workspaces) {
    crate::rows::store::scan_disk(workspaces, false).unwrap();
    let default = Workspace::from_label("local", PathBuf::from("/p/local"), false, "none".into(), String::new(), String::new());
    let _ = crate::rows::store::save(&default); // fails where a test made the registry folder unreadable
    let state_root = sot_platform::host::state_dir::sot_state_dir().unwrap();
    sweep_state_folders_at(&state_root, workspaces, later());
}

#[test]
fn a_start_under_another_host_name_keeps_a_row_another_registry_names() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (_id, folder, _toml) = registered_row();
    std::env::set_var("SOT_SELF_HOST", "laptop-2");
    std::fs::create_dir_all(crate::rows::store::app_config_dir().join("workspaces-laptop-2")).unwrap();
    let reg = Workspaces::new();
    assert_eq!(crate::rows::store::scan_disk(&reg, false).unwrap(), 0);
    start(&reg);
    assert!(folder.is_dir(), "a row registered under workspaces-laptop keeps its state folder");
}

#[test]
fn a_row_no_registry_names_loses_its_old_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (_id, folder, toml) = registered_row();
    std::fs::remove_file(toml).unwrap();
    start(&Workspaces::new());
    assert!(!folder.exists());
}

#[test]
fn a_start_with_no_registry_folder_of_its_own_removes_no_unnamed_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (_id, folder, _toml) = registered_row();
    std::env::set_var("SOT_SELF_HOST", "laptop-3");
    start(&Workspaces::new());
    assert!(folder.is_dir());
}

#[cfg(unix)]
#[test]
fn an_unreadable_registry_folder_removes_no_folder() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (_id, folder, toml) = registered_row();
    let dir = toml.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let log = sot_log::test_log::capture();
    start(&Workspaces::new());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(folder.is_dir(), "a registry read failure removes nothing");
    assert!(log.text().contains("could not all be read"), "{}", log.text());
}

#[cfg(unix)]
#[test]
fn an_unreadable_toml_removes_no_folder() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (_id, folder, toml) = registered_row();
    std::fs::set_permissions(&toml, std::fs::Permissions::from_mode(0o000)).unwrap();
    start(&Workspaces::new());
    std::fs::set_permissions(&toml, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(folder.is_dir(), "one unreadable toml removes nothing");
}

#[test]
fn a_toml_naming_the_id_but_missing_a_key_keeps_the_rows_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (id, folder, toml) = registered_row();
    let text = std::fs::read_to_string(&toml).unwrap();
    let damaged: String = text.lines().filter(|l| !l.starts_with("project_root")).map(|l| format!("{l}\n")).collect();
    std::fs::write(&toml, damaged).unwrap();
    start(&Workspaces::new());
    assert!(folder.is_dir(), "a toml that still names {id} did not keep its folder");
}

#[test]
fn a_legacy_toml_naming_the_id_keeps_the_rows_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (id, folder, toml) = registered_row();
    std::fs::remove_file(&toml).unwrap();
    let legacy = crate::rows::store::app_config_dir().join("sessions-laptop");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("old.toml"), format!("[backend]\nsession_id = \"{id}\"\n")).unwrap();
    start(&Workspaces::new());
    assert!(folder.is_dir());
}

#[cfg(unix)]
#[test]
fn another_hosts_registry_folder_that_cannot_be_stat_ed_keeps_the_rows_folder() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (_id, folder, toml) = registered_row();
    let config = crate::rows::store::app_config_dir();
    let away = tmp.path().join("away");
    let hidden = away.join("workspaces-laptop");
    std::fs::create_dir_all(&away).unwrap();
    std::fs::rename(toml.parent().unwrap(), &hidden).unwrap();
    std::os::unix::fs::symlink(&hidden, config.join("workspaces-laptop")).unwrap();
    std::fs::set_permissions(&away, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::env::set_var("SOT_SELF_HOST", "laptop-2");
    std::fs::create_dir_all(config.join("workspaces-laptop-2")).unwrap();
    start(&Workspaces::new());
    std::fs::set_permissions(&away, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(folder.is_dir(), "a registry folder that could not be stat'ed was skipped and its row's folder removed");
}

#[test]
fn a_start_with_a_fresh_config_folder_beside_an_old_state_root_removes_no_row_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (_id, folder, _toml) = registered_row();
    std::fs::remove_dir_all(crate::rows::store::app_config_dir()).unwrap();
    start(&Workspaces::new());
    assert!(folder.is_dir(), "a first start under a new config folder removed the old state root's folders");
}

#[test]
fn a_toml_with_a_section_above_the_id_still_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (id, folder, toml) = registered_row();
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(&toml, format!("[nav_state]\nx = \"1\"\n{text}")).unwrap();
    std::fs::write(&toml, std::fs::read_to_string(&toml).unwrap().replace("project_root", "project_rooot")).unwrap();
    start(&Workspaces::new());
    assert!(folder.is_dir(), "an id below a [section] line did not keep {id}'s folder");
}

#[cfg(unix)]
#[test]
fn a_changed_config_folder_removes_no_row_folder_on_this_start_or_the_next() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = pinned(tmp.path(), "laptop");
    let (_id, folder, _toml) = registered_row();
    start(&Workspaces::new()); // a whole start under the first config folder records it
    assert!(folder.is_dir());
    std::env::set_var("XDG_CONFIG_HOME", tmp.path().join("other-config"));
    start(&Workspaces::new());
    assert!(folder.is_dir(), "the first start under a new config folder removed a folder");
    start(&Workspaces::new());
    assert!(folder.is_dir(), "the second start under a new config folder removed a folder");
}
