// The window matches a `preview.changed` path from another row's watcher (an umbrella row over the same tree)
// against the active row's root from this list, as strings (`resolve_preview_changed`,
// rust/frontend/src/ui/preview/pane.rs). A watcher reports paths under the root its `FilesMode` resolved, so a row
// created in another spelling of its folder (through a link above it, as macOS's /tmp and /var/folders are; in
// another case or with an 8.3 name on Windows) is spelled two ways, and the root the window matches with must be
// the watcher's: `canonical_root`, or `project_root` from a daemon that sends none.
use super::*;
use crate::files::watcher::PreviewChanged;
use std::time::{Duration, Instant};

const VARS: [&str; 2] = ["SOT_COMM_HOME", "SOT_SELF_HOST"];

/// The test's folder, and the two variables the list's registry read and host name come from, set for the test:
/// on drop the folder is removed and the variables restored.
struct Scratch {
    dir: std::path::PathBuf,
    saved: Vec<Option<std::ffi::OsString>>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for (key, val) in VARS.iter().zip(&self.saved) {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn the_root_the_window_matches_with_holds_the_watchers_paths() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = VARS.iter().map(std::env::var_os).collect();
    let dir = std::env::temp_dir().join(format!(
        "sot-canonical-root-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let scratch = Scratch { dir, saved, _serial };
    std::fs::create_dir_all(scratch.dir.join("real").join("repo")).unwrap();
    // The row's folder in a second spelling: through a link on Unix, in upper case on Windows.
    #[cfg(unix)]
    let created = {
        std::os::unix::fs::symlink("real", scratch.dir.join("link")).unwrap();
        scratch.dir.join("link").join("repo")
    };
    #[cfg(windows)]
    let created = std::path::PathBuf::from(scratch.dir.join("real").join("repo").to_string_lossy().to_uppercase());
    std::env::set_var("SOT_COMM_HOME", &scratch.dir);
    std::env::set_var("SOT_SELF_HOST", "host-4");
    let (tx, mut rx) = tokio::sync::broadcast::channel::<PreviewChanged>(64);
    let workspaces = Workspaces::new();
    workspaces.set_watch_bus(tx);
    workspaces.insert(Workspace::from_label("linked", created.clone(), false, "none".into(), String::new(), String::new()));

    // The watcher registers its folders on a thread of its own: write until it reports the file.
    let deadline = Instant::now() + Duration::from_secs(20);
    let event = loop {
        std::fs::write(created.join("a.txt"), b"x").unwrap();
        match tokio::time::timeout(Duration::from_millis(250), rx.recv()).await {
            Ok(Ok(c)) if c.path.ends_with("a.txt") => break c,
            _ if Instant::now() < deadline => continue,
            _ => panic!("the row's watcher reported no change to a.txt in 20 s"),
        }
    };
    let out = handle_workspace_list(1, json!({}), &workspaces, &no_pluto()).await.expect("handler must not error");
    let entry = out[0].0.payload["workspaces"][0].clone();
    let listed = |f: &str| entry.get(f).and_then(|v| v.as_str()).map(str::to_string);
    let under = |root: &str, path: &str| path.strip_prefix(root).is_some_and(|rest| rest.starts_with(['/', '\\']));
    let path = event.path.to_string_lossy().into_owned();
    let project_root = listed("project_root").expect("every entry has a project_root");
    assert!(
        !under(&project_root, &path),
        "premise: the watcher spells {path} as its folder resolves, so the row's root as created ({project_root}) is a second spelling"
    );
    let root = listed("canonical_root").unwrap_or(project_root.clone());
    assert!(
        under(&root, &path),
        "the watcher's path {path} is not under the root the window matches with, {root} (project_root {project_root}, canonical_root {:?})",
        listed("canonical_root")
    );
    drop(workspaces);
}
