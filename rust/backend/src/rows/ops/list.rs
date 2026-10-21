//! workspace.list (the rows the window shows, with their comm state) and workspace.activate (viewing clears done).

use anyhow::Context;
use anyhow::Result;
#[cfg(test)]
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use crate::server::reply::HandlerOutput;
use crate::comm::registry::registry::{clear_comm_unread, comm_handle_for_workspace, host_matches, read_comm_agents};
use crate::rows::Workspaces;
#[cfg(test)]
use crate::rows::Workspace;

/// A Pluto handle that never started a server, for the tests that list rows.
#[cfg(test)]
fn no_pluto() -> crate::sidecars::pluto::Pluto {
    crate::sidecars::pluto::Pluto::new(std::path::PathBuf::new(), std::path::PathBuf::new())
}

pub async fn handle_workspace_list(
    req_id: u64,
    _payload_json: serde_json::Value,
    workspaces: &Workspaces,
    pluto: &crate::sidecars::pluto::Pluto,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceListEntry, WorkspaceListRes};
    let default_id = workspaces.default_id();
    // Read the sot-comm registry once per list call (fresh — picks up the
    // owning agents' latest `comm-status.sh` writes). `None` when the file is
    // absent/malformed; every lookup below then falls back to empty strings.
    // On a blocking thread: the read's retry sleeps after a failed read.
    let comm_agents = tokio::task::spawn_blocking(read_comm_agents).await.ok().flatten();
    let host = crate::rows::store::declared_host();
    // Pull `.agents[agent_name].<field>` as an owned String, "" if anything is
    // missing or not a string. LU5d2: `agent_name` here is a handle the caller
    // (below) already bound to THIS workspace — by the
    // stored `agent_name` fallback — never proof it's this host's row, so
    // filter the entry through `host_matches` too: a same-named handle
    // stamped by another host on the shared registry must read as empty, not
    // leak its summary/status_at/state into this host's list.
    let agent_str = |agent_name: &str, field: &str| -> String {
        if agent_name.is_empty() {
            return String::new();
        }
        comm_agents
            .as_ref()
            .and_then(|a| a.get(agent_name))
            .filter(|entry| host_matches(entry, &host))
            .and_then(|entry| entry.get(field))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let ws_list = workspaces.list();
    // Pure memory: kept current by the row's lifecycle observer, no lane query.
    let mut entries: Vec<WorkspaceListEntry> = ws_list
        .iter()
        .map(|ws| {
            // Which registry row is this workspace's — one rule, shared with
            // `clear_comm_unread` (`comm_handle_for_workspace`).
            let handle = comm_handle_for_workspace(ws, &ws_list);
            // The registry `state` IS the badge — no pane scrape, no merge, no
            // precedence to arbitrate. A prior version of this comment described
            // a registry-vs-pane precedence merge that no longer exists here; it
            // was the only thing in the tree suggesting a pane-based idle
            // detector could overrule a stamped fact, which it never can (only
            // an act that could BE the answer clears a question — ADR 0044
            // amendment, field report 2026-09-27).
            let agent_state = agent_str(&handle, "state");
            // `phase` is read straight off the row's own cell — every row
            // is a capsule on this build (`ws.runtime` is always
            // `"capsule"`), so there is no other case left to branch on.
            // A row with no state dir at all reads NEVER_STARTED_PHASE
            // ("stopped") exactly like a row that simply hasn't been
            // attached yet — deliberately NOT distinguished here (Fable
            // review, `tests/capsule_workspaces/`'s own "Rule H": a pre-seeded,
            // never-`workspace.create`d row is indistinguishable from a
            // truly orphaned one by any fact this list can cheaply check,
            // and a wire-visible claim otherwise would be dishonest for
            // exactly that row shape). `rows::run::resume::
            // log_orphaned_state_dirs` still names such rows once at
            // boot, as an operator diagnostic only; `workspace.destroy`'s
            // own real proof (a live lane connect) is what actually
            // decides whether one is removable.
            let state_dir = sot_log::host::state_dir::sot_state_dir().map(|root| {
                crate::rows::spawn::state_root::state_dir_for(&root, &ws.workspace_id)
                    .to_string_lossy()
                    .into_owned()
            });
            let phase = Some(ws.phase().as_wire_str().to_string());
            WorkspaceListEntry {
                workspace_id: ws.workspace_id.clone(),
                slug: ws.slug.clone(),
                label: ws.label.clone(),
                project_root: ws.project_root.to_string_lossy().into_owned(),
                canonical_root: ws.canonical_root().map(|r| r.to_string_lossy().into_owned()),
                session_name: ws.session_name.clone(),
                kernel_running: ws.kernel_built(),
                is_default: default_id.as_deref() == Some(ws.workspace_id.as_str()),
                autostart_claude: ws.autostart_claude,
                agent: ws.agent(),
                agent_name: if handle.is_empty() {
                    ws.agent_name()
                } else {
                    handle.clone()
                },
                agent_handle: ws.agent_handle(),
                task: ws.task.clone(),
                agent_state,
                agent_summary: agent_str(&handle, "summary"),
                agent_status_at: agent_str(&handle, "status_at"),
                repl_state: ws.repl_state().to_string(),
                runtime: ws.runtime.clone(),
                state_dir,
                phase,
                activation_error: ws.activation_error(),
                account: ws.account(),
            }
        })
        .collect();
    // Pin the default workspace (the daemon's home anchor) FIRST: the FE never
    // lists it, but its position is the strip's own active-index fallback.
    // Stable sort: every other workspace keeps its alphabetical-by-slug order.
    entries.sort_by(|a, b| b.is_default.cmp(&a.is_default));
    tracing::debug!(count = entries.len(), "workspace.list");
    let res = WorkspaceListRes {
        workspaces: entries,
        pluto_server_up: pluto.server_up().await,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_LIST, serde_json::to_value(res)?),
        None,
    )])
}

/// `workspace.activate` — builds the ack. The connection-local state this
/// updates (`active_workspace`, `server/conn.rs`'s `handle_connection`) is
/// mutated by the CALLER, not here — this function only resolves
/// `req.workspace_id` (again; the caller does the same resolve to learn
/// what to store) and echoes back the canonical id,
/// or `None` when it didn't resolve. See `op::WORKSPACE_ACTIVATE` (ops/mod.rs)
/// for the full design.
pub async fn handle_workspace_activate(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceActivateReq, WorkspaceActivateRes};
    let req: WorkspaceActivateReq =
        serde_json::from_value(payload_json).context("workspace.activate payload")?;
    let resolved_ws = workspaces.resolve(req.workspace_id.as_deref());
    let resolved = resolved_ws.as_ref().map(|ws| ws.workspace_id.clone());
    // `read: true` = a PERSON switched the view here (Sessions-Enter,
    // Shift+Left/Right cycling) — clear this row's blue (ADR 0044). The ack
    // below is sent unconditionally, whatever this does or doesn't clear.
    if req.read {
        if let Some(ws) = resolved_ws.clone() {
            let host = crate::rows::store::declared_host();
            let rows = workspaces.list();
            let _ = tokio::task::spawn_blocking(move || clear_comm_unread(&ws, &rows, &host)).await;
        }
    }
    tracing::info!(
        requested = req.workspace_id.as_deref().unwrap_or("<default>"),
        resolved = resolved.as_deref().unwrap_or("<unresolved>"),
        read = req.read,
        "workspace.activate"
    );
    let res = WorkspaceActivateRes {
        workspace_id: resolved,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_ACTIVATE, serde_json::to_value(res)?),
        None,
    )])
}

#[cfg(test)]
#[path = "list_workspace_activate_read_tests.rs"]
mod workspace_activate_read_tests;

#[cfg(test)]
#[path = "list_agent_str_host_filter_tests.rs"]
mod agent_str_host_filter_tests;

#[cfg(test)]
#[path = "list_canonical_root_tests.rs"]
mod canonical_root_tests;
