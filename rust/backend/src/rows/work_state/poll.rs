//! The work-state poll: what every row shows, compared every [`EVERY`], and a `workspace.changed` (action
//! `agent_state`) when any row's changed, so the window re-lists. A poll, not a watch: the registry can live on a
//! network filesystem where file notifications are unreliable, and the period also coalesces a busy turn's restamps.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::broadcast;

use super::{shown, Shown};
use crate::rows::{Workspace, WorkspaceChanged, Workspaces};

const EVERY: Duration = Duration::from_millis(1500);

/// What each of `rows` shows, by id: the one read ([`shown`]) the list makes too, so the poll publishes exactly when
/// what the window would show changed, and never for another host's entry or a field the window does not show.
pub(crate) fn project(rows: &[Arc<Workspace>], agents: Option<&Value>, host: &str) -> Vec<(String, Shown)> {
    rows.iter().map(|ws| (ws.workspace_id.clone(), shown(ws, rows, agents, host))).collect()
}

/// Starts the poll. A registry that cannot be read this tick is skipped, not taken for one with no entries.
pub(crate) fn spawn(workspaces: Workspaces, ws_events_tx: &broadcast::Sender<WorkspaceChanged>) {
    let tx = ws_events_tx.clone();
    tokio::spawn(async move {
        let mut last: Option<Vec<(String, Shown)>> = None;
        let mut tick = tokio::time::interval(EVERY);
        loop {
            tick.tick().await;
            let rows = workspaces.list();
            let Ok(Some(agents)) = tokio::task::spawn_blocking(crate::comm::registry::registry::read_comm_agents).await else {
                continue;
            };
            let cur = project(&rows, Some(&agents), &crate::rows::store::declared_host());
            if last.as_ref().is_some_and(|p| *p != cur) {
                let _ = tx.send(WorkspaceChanged { action: "agent_state".into(), slug: String::new(), workspace_id: String::new() });
            }
            last = Some(cur);
        }
    });
}

#[cfg(test)]
#[path = "poll_tests.rs"]
mod tests;
