use serde_json::json;

use super::*;
use crate::rows::Workspace;

/// A save through a link at a folder of its path answers `bad_node_id` and writes nothing.
#[cfg(unix)]
#[tokio::test]
async fn a_save_through_a_folder_link_answers_bad_node_id() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("lnk")).unwrap();
    let workspaces = Workspaces::new();
    workspaces.insert(Workspace::from_label("p", root.path().to_path_buf(), false, "none".into(), String::new(), String::new()));
    let req = json!({ "node_id": "files:lnk/a.txt", "content": "x", "workspace_id": "p" });
    let out = handle_file_write(7, req, &Session::new(), &workspaces).await.unwrap();
    assert_eq!(out[0].0.payload["code"], "bad_node_id", "{}", out[0].0.payload);
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}
