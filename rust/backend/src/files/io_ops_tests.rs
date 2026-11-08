#![cfg(unix)]

use serde_json::json;

use super::*;
use crate::rows::Workspace;

/// A save through a link at a folder of its path answers `bad_node_id` and writes nothing.
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

/// A folder made through a link at a folder of its path answers `bad_node_id` and creates nothing; a plain one is made.
#[tokio::test]
async fn a_new_folder_through_a_folder_link_answers_bad_node_id() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("lnk")).unwrap();
    std::fs::create_dir(root.path().join("plain")).unwrap();
    let workspaces = Workspaces::new();
    workspaces.insert(Workspace::from_label("p", root.path().to_path_buf(), false, "none".into(), String::new(), String::new()));
    let ask = |id: &str| json!({ "node_id": id, "workspace_id": "p" });
    let out = handle_dir_create(7, ask("files:lnk/new"), &Session::new(), &workspaces).await.unwrap();
    assert_eq!(out[0].0.payload["code"], "bad_node_id", "{}", out[0].0.payload);
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    let out = handle_dir_create(8, ask("files:lnk"), &Session::new(), &workspaces).await.unwrap();
    assert_eq!(out[0].0.payload["code"], "already_exists", "{}", out[0].0.payload);
    let out = handle_dir_create(9, ask("files:plain/new"), &Session::new(), &workspaces).await.unwrap();
    assert!(out[0].0.payload.get("error").is_none(), "{}", out[0].0.payload);
    assert!(root.path().join("plain/new").is_dir());
}
