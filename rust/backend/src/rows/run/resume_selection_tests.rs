use super::*;
use crate::agents::selection::FILE_NAME;
use crate::rows::Workspace;

/// The walk clears a registered row's selection file that no connection of this process set, whatever the row's runtime.
#[tokio::test]
async fn the_walk_clears_each_registered_rows_unowned_selection() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new();
    let mut row = Workspace::from_label("walk", project.path().to_path_buf(), false, "claude".into(), String::new(), String::new());
    row.workspace_id = "ws-walk-1".to_string();
    row.runtime = "other".to_string();
    workspaces.insert(row);
    let file = super::super::state_dir_for(state.path(), "ws-walk-1").join(FILE_NAME);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "from an earlier process").unwrap();
    resume_all(state.path().to_path_buf(), workspaces).await;
    assert!(!file.exists(), "the walk removes a file no live connection set");
}
