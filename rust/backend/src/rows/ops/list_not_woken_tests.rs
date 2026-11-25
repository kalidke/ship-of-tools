// Which rows `workspace.list` marks `not_woken_by_mail`: a Codex session in phase `ready` whose state folder holds neither
// the hook marker nor a binding. Every other row is left out, each for its own rule.
use super::*;
use crate::rows::workspace::{Observation, SupervisorIdentity};
use std::path::PathBuf;

fn row(label: &str, agent: &str, ready: bool) -> Arc<Workspace> {
    let mut ws = Workspace::from_label(label, PathBuf::from("/tmp").join(label), false, agent.to_string(), String::new(), String::new());
    ws.runtime = "capsule".to_string();
    let ws = Arc::new(ws);
    if ready {
        let observation = Observation::Phase { phase: Phase::Ready, supervisor: SupervisorIdentity { pid: 7, created: 7 }, run: None };
        assert!(ws.apply_phase_observation(observation));
    }
    ws
}

/// Each row's state folder holds nothing, so the fact of the state folder holds for all of them; only the Ready Codex row
/// is marked. Red if the Ready condition, the Codex condition or the folder's marker and binding are not read.
#[test]
fn only_a_ready_codex_session_with_neither_marker_nor_binding_is_marked() {
    let root = tempfile::tempdir().unwrap();
    let (ready, stopped, claude, hooked, bound) = (row("ready", "codex", true), row("stopped", "codex", false), row("claude", "claude", true), row("hooked", "codex", true), row("bound", "codex", true));
    let rows = [ready.clone(), stopped.clone(), claude.clone(), hooked.clone(), bound.clone()];
    for ws in &rows {
        std::fs::create_dir_all(crate::rows::spawn::state_root::state_dir_for(root.path(), &ws.workspace_id)).unwrap();
    }
    let folder = |ws: &Workspace| crate::rows::spawn::state_root::state_dir_for(root.path(), &ws.workspace_id);
    std::fs::write(folder(&hooked).join(crate::agents::selection::MARKER_NAME), "").unwrap();
    std::fs::write(folder(&bound).join(crate::agents::env::CODEX_THREAD_FILE), "aaaaaaaa-1111-4222-8333-444444444444\n").unwrap();
    assert_eq!(not_woken_ids(&rows, root.path()), std::collections::HashSet::from([ready.workspace_id.clone()]));
    assert!(not_woken_ids(&[], root.path()).is_empty());
}
