//! Fixtures shared by the nav tests and the tests of the window around them.

use super::*;

pub(in crate::ui) fn node(id: &str, label: &str, has_children: bool) -> TreeNode {
    TreeNode {
        id: id.to_string(),
        label: label.to_string(),
        kind: "files".to_string(),
        has_children,
        badges: Vec::new(),
        payload: Default::default(),
    }
}

/// Minimal `WorkspaceInfo` for cache/tree tests — every field a real
/// `workspace.list` row carries, defaulted to the empty/false case so
/// each test only names what it cares about.
pub(in crate::ui) fn ws_info(slug: &str, session_name: &str) -> crate::transport::WorkspaceInfo {
    crate::transport::WorkspaceInfo {
        workspace_id: format!("ws-{slug}-0000"),
        slug: slug.to_string(),
        label: String::new(),
        project_root: format!("/projects/{slug}"),
        session_name: session_name.to_string(),
        kernel_running: false,
        is_default: false,
        agent: String::new(),
        autostart_claude: false,
        agent_name: String::new(),
        agent_handle: String::new(),
        task: String::new(),
        agent_state: String::new(),
        agent_summary: String::new(),
        agent_status_at: String::new(),
        repl_state: String::new(),
        runtime: String::new(),
        phase: None,
        account: String::new(),
    }
}
