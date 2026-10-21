//! agent.join: a session declares its comm handle on its row, under the row's guard.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::AgentJoinReq;
use sot_protocol::AgentJoinRes;
use sot_protocol::Frame;
use crate::comm::registry::liveness::unended_handles;
use crate::rows::WorkspaceChanged;
use crate::rows::Workspaces;
use tokio::sync::broadcast;
use crate::paths::valid_name;
use crate::server::reply::HandlerOutput;

/// `agent.join` (ADR 0046 decision 1): a session inside `req.workspace_id`
/// declares its sot-comm handle to this daemon — read by every later
/// `workspace.list`/`clear_comm_unread` call ahead of the self-file
/// read-back fallback (`capsule_comm_handle`, S5: stays until family H).
/// `handle` is validated against the same charset `workspace.create`'s own
/// `valid_name` already enforces (comm-lib.sh's derived handles are built
/// to satisfy exactly this). `set_agent_handle` (S6) is a guarded
/// in-place update of the existing row — never a replacement, never a
/// resurrection of a destroyed one. `ok: true` only after
/// `Workspace.agent_handle` is durably persisted (S7): a save failure is
/// reported as `persist_failed`, not swallowed as a warning, since a
/// declared handle with nothing on disk would not survive a daemon
/// restart. Publishes `workspace.changed` so the FE re-lists on success.
/// Refuses `bad_handle` for an invalid handle, `unknown_workspace` for a
/// workspace this daemon doesn't have (destroyed or never existed).
///
/// An empty `workspace_id` is a join from outside any row (`outside_join`): it
/// changes nothing and answers `ok` only when no row this daemon has not seen end
/// holds the handle, else `held_by_row`. That is how a session outside every row
/// is told the handle is not its to take.
///
/// A newer declaration MOVES the handle (ADR 0049, B5): `set_agent_handle`
/// clears it from every other row that held it, in memory, and this handler
/// logs one line per move. The reply follows the joining row's own save
/// alone. Whatever that save did, the rows that lost the handle are saved
/// after the reply, each under its own guard if still registered
/// (`persist_moved`, in a spawned task, one guard at a time); a failed save
/// there is a warning.
pub async fn handle_agent_join(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
    ws_events_tx: &broadcast::Sender<WorkspaceChanged>,
) -> Result<HandlerOutput> {
    Ok(join_row(req_id, payload_json, workspaces, ws_events_tx).await?.0)
}

/// The join itself: the reply, and the task saving the rows that lost the
/// handle (`None` when none did, or the join was refused). The handler
/// lets that task run on its own; tests await it.
async fn join_row(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
    ws_events_tx: &broadcast::Sender<WorkspaceChanged>,
) -> Result<(HandlerOutput, Option<tokio::task::JoinHandle<()>>)> {
    let req: AgentJoinReq = serde_json::from_value(payload_json).context("agent.join payload")?;
    if !valid_name(&req.handle) {
        return Ok((vec![(
            Frame::res(
                req_id,
                op::AGENT_JOIN,
                json!({
                    "error": format!("invalid handle: {:?}", req.handle),
                    "code": "bad_handle",
                }),
            ),
            None,
        )], None));
    }
    if req.workspace_id.is_empty() {
        return Ok((vec![(outside_join(req_id, &req.handle, workspaces).await?, None)], None));
    }
    // Round 2 (Codex finding B1): take the SAME per-row
    // lifecycle guard `destroy_capsule_workspace` takes (ADR 0043
    // decision 33) — held across the mutate+persist below, exactly the
    // way destroy holds it across its own toml-delete + registry-removal
    // sequence, so the two can never interleave. Without this, a
    // concurrent destroy could delete the toml and remove the registry
    // entry AFTER `set_agent_handle`'s own read but BEFORE this save,
    // and the save would recreate the toml for a workspace_id the
    // registry no longer has — a resurrection destroy's caller believes
    // it prevented. `capsule_guard` mints/returns `None` under its own
    // write lock if the row isn't currently registered, so a row already
    // gone by the time we ask for the guard refuses immediately, same as
    // before.
    let Some(guard) = workspaces.capsule_guard(&req.workspace_id) else {
        return Ok((vec![(
            Frame::res(
                req_id,
                op::AGENT_JOIN,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )], None));
    };
    let held = guard.lock().await;
    // Re-check under the guard: `capsule_guard`'s own registration check
    // ran before we actually acquired the lock above — a destroy that
    // was already mid-flight (holding this same guard) could have
    // finished removing the row in the interim. `set_agent_handle` does
    // its own fresh lookup, so this one call is both the re-check and
    // the mutation.
    let Some((ws, moved)) = workspaces.set_agent_handle(&req.workspace_id, &req.handle) else {
        return Ok((vec![(
            Frame::res(
                req_id,
                op::AGENT_JOIN,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )], None));
    };
    // (S7, Codex finding B9): `ok` depends on the save
    // actually succeeding — a persisted daemon restart with no toml
    // record would otherwise lose the only authoritative handle (the
    // self-file read-back this replaces is a fallback, not a second
    // source of truth to fall back on for a row that DID declare).
    // `set_agent_handle` has already applied the in-memory update by this
    // point (guarded in-place, S6) — a save failure is reported, not
    // rolled back, so the caller can retry the SAME join rather than
    // re-deriving a value that already matches memory. Still under
    // `held`: the save that recreates the toml must finish before a
    // waiting destroy can start deleting it.
    for from in &moved {
        tracing::info!(handle = %req.handle, from = %from, to = %req.workspace_id, "agent.join: the handle moved off an older row");
    }
    let saved = crate::rows::store::save(&ws);
    drop(held);
    // The rows that lost the handle are saved after the reply, whatever
    // the joiner's own save did: a failed save here must not leave their
    // tomls on the handle, and the reply must not wait on another row's
    // guard (a crash-looping row holds its for seconds).
    let saves = (!moved.is_empty()).then(|| spawn_persist_moved(workspaces.clone(), moved, req.handle.clone()));
    if let Err(e) = saved {
        tracing::warn!(error = %e, workspace_id = %req.workspace_id, "agent.join: toml persist failed");
        return Ok((vec![(
            Frame::res(
                req_id,
                op::AGENT_JOIN,
                json!({
                    "error": format!("{e:#}"),
                    "code": "persist_failed",
                }),
            ),
            None,
        )], saves));
    }
    tracing::info!(workspace_id = %req.workspace_id, handle = %req.handle, "agent.join");
    let _ = ws_events_tx.send(WorkspaceChanged {
        action: "agent_joined".into(),
        slug: ws.slug.clone(),
        workspace_id: ws.workspace_id.clone(),
    });
    Ok((vec![(
        Frame::res(
            req_id,
            op::AGENT_JOIN,
            serde_json::to_value(AgentJoinRes { ok: true })?,
        ),
        None,
    )], saves))
}

/// A join from outside any row (`workspace_id` empty): it takes nothing and writes nothing, and is refused
/// `held_by_row` while any row not latched ended holds `handle` (`unended_handles`). A row this daemon has not seen
/// running (loaded from disk, Unreachable, Foreign, Ending) may still run a session, so only a destroyed or latched
/// row frees its handle. The answer is the daemon's, from its own rows, never from the registry file's binding.
async fn outside_join(req_id: u64, handle: &str, workspaces: &Workspaces) -> Result<Frame> {
    let rows = workspaces.list();
    let held = tokio::task::spawn_blocking(move || unended_handles(&rows)).await.context("agent.join rows")?;
    if held.contains(handle) {
        tracing::info!(handle = %handle, "agent.join: refused a join from outside any row; a row not seen ended holds the handle");
        return Ok(Frame::res(
            req_id,
            op::AGENT_JOIN,
            json!({
                "error": format!("@{handle} is held by a row; a session outside any row cannot join as it"),
                "code": "held_by_row",
            }),
        ));
    }
    Ok(Frame::res(req_id, op::AGENT_JOIN, serde_json::to_value(AgentJoinRes { ok: true })?))
}

/// Save the rows that lost `handle`, one at a time, in a task of their own.
fn spawn_persist_moved(workspaces: Workspaces, moved: Vec<String>, handle: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for from in &moved {
            persist_moved(&workspaces, from, &handle).await;
        }
    })
}

/// Persist a row that lost its handle to a newer `agent.join`, under that
/// row's own guard: a row destroyed meanwhile (no guard, or gone once the
/// guard is held) is left alone, so no save recreates its toml. The row is
/// re-resolved by id for the save: a re-inserted slug rebuilds the row and
/// blanks its handle, so the `Arc` the move saw may no longer be the
/// registered one. A failed save is a warning: the joiner cannot repair it.
async fn persist_moved(workspaces: &Workspaces, workspace_id: &str, handle: &str) {
    let Some(guard) = workspaces.capsule_guard(workspace_id) else {
        return;
    };
    let _held = guard.lock().await;
    let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
        return;
    };
    if let Err(e) = crate::rows::store::save(&ws) {
        tracing::warn!(error = %e, workspace_id = %workspace_id, handle = %handle, "agent.join: toml persist failed for a row that lost the handle");
    }
}

#[cfg(test)]
#[path = "join_agent_join_tests.rs"]
mod agent_join_tests;
