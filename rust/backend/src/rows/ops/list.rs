//! workspace.list (the rows the window shows, with their work-state) and workspace.activate (viewing clears done).

use anyhow::Context;
use anyhow::Result;
#[cfg(test)]
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use crate::server::reply::HandlerOutput;
use crate::comm::registry::registry::read_comm_agents;
use crate::rows::handle::bound_handle;
use crate::rows::work_state::{read_clear, shown_of};
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
    // The registry read once per list, fresh, on a blocking thread (its retry sleeps after a failed read); `None` when
    // it is absent or malformed, and every row then shows no work-state.
    let comm_agents = tokio::task::spawn_blocking(read_comm_agents).await.ok().flatten();
    let host = crate::rows::store::declared_host();
    let ws_list = workspaces.list();
    // Pure memory: kept current by the row's lifecycle observer, no lane query.
    let mut entries: Vec<WorkspaceListEntry> = ws_list
        .iter()
        .map(|ws| {
            // The row's handle and what it shows: work-state's one read (`work_state::shown_of`), through the one answer
            // to a row's handle (`bound_handle`), and only from an entry of this host.
            let handle = bound_handle(ws, &ws_list);
            let work = shown_of(ws, &ws_list, comm_agents.as_ref(), &host);
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
            let state_dir = sot_platform::host::state_dir::sot_state_dir().map(|root| {
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
                agent_handle: crate::rows::handle::declared_handle(ws),
                task: ws.task.clone(),
                agent_state: work.state,
                agent_summary: work.summary,
                agent_status_at: work.status_at,
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
            let _ = tokio::task::spawn_blocking(move || read_clear(&ws, &rows, &host)).await;
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
