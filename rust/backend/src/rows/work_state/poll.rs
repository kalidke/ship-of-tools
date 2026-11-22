//! The work-state poll: what every row shows and the time its terminal last took typed input, compared every
//! [`EVERY`], and a `workspace.changed` (action `agent_state`) when any row's changed, so the window re-lists. A poll,
//! not a watch: the registry can live on a network filesystem where file notifications are unreliable, and the period
//! also coalesces a busy turn's restamps.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::broadcast;

use super::{shown_of, Shown};
use crate::rows::{Workspace, WorkspaceChanged, Workspaces};

const EVERY: Duration = Duration::from_millis(1500);

/// What each of `rows` shows, by id: the one read ([`shown_of`]) the list makes too, so the poll publishes exactly when
/// what the window would show changed, and never for another host's entry or a field the window does not show.
pub(crate) fn project(rows: &[Arc<Workspace>], agents: Option<&Value>, host: &str) -> Vec<(String, Shown)> {
    rows.iter().map(|ws| (ws.workspace_id.clone(), shown_of(ws, rows, agents, host))).collect()
}

/// The second each of `rows`' terminals last took typed input, by id, from the daemon's memory (the observer's copy).
pub(crate) fn typed_of(rows: &[Arc<Workspace>]) -> Vec<(String, u64)> {
    rows.iter().map(|ws| (ws.workspace_id.clone(), ws.input_at())).collect()
}

/// What the poll compared last: what each row shows, from the last tick whose registry read, and each row's input time.
#[derive(Default)]
pub(crate) struct Seen {
    shown: Option<Vec<(String, Shown)>>,
    typed: Option<Vec<(String, u64)>>,
}

/// One tick's comparison: whether what a window shows or sorts by changed since the last. `shown` is `None` on a tick
/// whose registry could not be read: that half is skipped, not taken for a registry with no entries, and the input
/// times are compared all the same, so a computer with no registry still tells its windows of typing.
pub(crate) fn changed(seen: &mut Seen, shown: Option<Vec<(String, Shown)>>, typed: Vec<(String, u64)>) -> bool {
    let mut changed = seen.typed.as_ref().is_some_and(|p| *p != typed);
    seen.typed = Some(typed);
    if let Some(shown) = shown {
        changed |= seen.shown.as_ref().is_some_and(|p| *p != shown);
        seen.shown = Some(shown);
    }
    changed
}

/// The poll; runs forever. A registry that cannot be read this tick is skipped, not taken for one with no entries; the
/// input times are compared all the same ([`changed`]).
pub(crate) async fn run(workspaces: Workspaces, tx: broadcast::Sender<WorkspaceChanged>) {
    let mut seen = Seen::default();
    let mut tick = tokio::time::interval(EVERY);
    loop {
        tick.tick().await;
        let rows = workspaces.list();
        let shown = match tokio::task::spawn_blocking(crate::comm::registry::registry::read_comm_agents).await {
            Ok(Some(agents)) => Some(project(&rows, Some(&agents), &crate::rows::store::declared_host())),
            _ => None,
        };
        if changed(&mut seen, shown, typed_of(&rows)) {
            let _ = tx.send(WorkspaceChanged { action: "agent_state".into(), slug: String::new(), workspace_id: String::new() });
        }
    }
}

#[cfg(test)]
#[path = "poll_tests.rs"]
mod tests;
