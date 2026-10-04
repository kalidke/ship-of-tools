//! workspace.destroy: end a row's run, then remove the row; the default row ends its run and stays.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use crate::server::reply::HandlerOutput;
use crate::rows::run::end::{CapsuleDestroyOutcome, ALREADY_REMOVED, capsule_end_not_reached_payload, default_row_end_response, destroy_capsule_workspace, remove_row_files};
use crate::rows::anchor::end_default_row_run;
use crate::comm::registry::registry::remove_comm_agents_for_workspace;
use crate::session::Session;
use crate::rows::WorkspaceChanged;
use crate::rows::Workspaces;
use tokio::sync::broadcast;

pub async fn handle_workspace_destroy(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceDestroyReq, WorkspaceDestroyRes};
    let req: WorkspaceDestroyReq =
        serde_json::from_value(payload_json).context("workspace.destroy payload")?;
    tracing::info!(workspace_id = %req.workspace_id, "workspace.destroy");

    let Some(ws) = workspaces.resolve(Some(&req.workspace_id)) else {
        let payload = json!({
            "error": format!("unknown workspace: {}", req.workspace_id),
            "code": "unknown_workspace",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_DESTROY, payload),
            None,
        )]);
    };

    // The default workspace's ROW is never destroyed here — it's the
    // daemon's anchor, with no fallback target to swap ops to. A default
    // CAPSULE row instead ends its run and keeps the row,
    // reusing the non-default delete's own path below — a `Kept`
    // (unconfirmed) outcome still returns the SAME typed error, never a
    // fabricated success.
    if workspaces.default_id().as_deref() == Some(ws.workspace_id.as_str()) {
        // Same end-run path the non-default delete uses below. The
        // reason is honest for THIS row (not "deleted" — it's kept).
        let (outcome, held_guard) = destroy_capsule_workspace(
            &ws.workspace_id,
            "run ended by the user",
            &ws.agent(),
            &ws.agent_name(),
            &ws.slug,
            &ws.project_root,
            workspaces,
            true,
        )
        .await;
        let (payload, confirmed_ended) =
            default_row_end_response(&ws.workspace_id, &ws.slug, &ws.label, outcome);
        tracing::info!(workspace_id = %ws.workspace_id, confirmed_ended, "workspace.destroy: default row's capsule run outcome; row kept");

        // The row guard (if any) rides along into the reset below and
        // drops only once that returns — see `end_default_row_run`'s own
        // doc.
        end_default_row_run(
            workspaces,
            ws_events,
            &ws.workspace_id,
            &ws.slug,
            &ws.agent_name(),
            confirmed_ended,
            held_guard,
        )
        .await;

        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_DESTROY, payload),
            None,
        )]);
    }

    let slug = ws.slug.clone();
    let label = ws.label.clone();
    let workspace_id = ws.workspace_id.clone();
    let agent_name = ws.agent_name();

    // This row's guard, taken below by whichever arm runs — HELD (ADR
    // 0043 decision 33, Codex review round 2) across the removal below, past the `if`, so neither a
    // watchdog restart nor a racing `agent.join` can land between a
    // confirmed end and `remove_by_id`. Dropped explicitly once removal
    // is done; stays `None` only for a `Kept` outcome (nothing is
    // removed) or a row that was already gone by the time its arm asked.

    // ADR 0042 slice L1a, Codex review finding 3: a capsule whose run did NOT reach `record_closed`/
    // `record_verified` STOPS the whole delete here: the row and its
    // toml are kept, and the caller sees a typed error, so a live or
    // unreachable run is never orphaned by a delete that silently
    // "succeeded" out from under it.
    let destroy_guard: Option<tokio::sync::OwnedMutexGuard<()>> = {
        let reason = format!("workspace '{slug}' deleted");
        let (outcome, held) = destroy_capsule_workspace(
            &workspace_id,
            &reason,
            &ws.agent(),
            &agent_name,
            &slug,
            &ws.project_root,
            workspaces,
            true,
        )
        .await;
        match outcome {
            CapsuleDestroyOutcome::Removable(outcome) => {
                tracing::info!(workspace_id = %workspace_id, %outcome, "workspace.destroy: capsule run ended; removing the row");
                held
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                tracing::warn!(workspace_id = %workspace_id, detail = %detail, "workspace.destroy: capsule run not confirmed ended; keeping the row");
                return Ok(vec![(
                    Frame::res(
                        req_id,
                        op::WORKSPACE_DESTROY,
                        capsule_end_not_reached_payload(&detail),
                    ),
                    None,
                )]);
            }
            CapsuleDestroyOutcome::AlreadyRemoved => {
                tracing::info!(workspace_id = %workspace_id, "workspace.destroy: already removed by another end");
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_DESTROY, capsule_end_not_reached_payload(ALREADY_REMOVED)),
                    None,
                )]);
            }
        }
    };

    // Prune the sot-comm registry. Ending the capsule run takes the agent
    // down before it can run its own comm-leave, so the killer must deregister
    // it — otherwise its row lingers as a ghost in `workspace.list`, which
    // merges the registry. Drop exactly the rows this
    // workspace owned: by stored `agent_name`, and by the row's own
    // `workspace_id` (covers a manually-joined handle, e.g. `comm-join.sh
    // --name other`, whose `ws.agent_name` was never set). Best-effort +
    // blocking (fs + file lock) → spawn_blocking, non-fatal like the row
    // teardown above.
    let reg_agent = agent_name.clone();
    let reg_ws = workspace_id.clone();
    let reg_host = crate::rows::store::declared_host();
    let comm_removed = tokio::task::spawn_blocking(move || {
        remove_comm_agents_for_workspace(&reg_agent, &reg_ws, &reg_host)
    })
    .await
    .unwrap_or_default();
    if !comm_removed.is_empty() {
        tracing::info!(
            removed = ?comm_removed,
            slug = %slug,
            "pruned sot-comm registry rows for destroyed workspace"
        );
    }

    // Best-effort: a remove error is logged + reported but doesn't block
    // the in-memory removal.
    let toml_removed = remove_row_files(&slug);

    // Drop from in-memory registry last. The Arc<Workspace> dropped
    // here is also the one holding the kernel/repl handles; when the
    // last Arc dies their Drop impls run and the Julia children are
    // killed. Other Arc holders (e.g. mid-flight handlers) will keep
    // those processes alive until they finish.
    let _ = workspaces.remove_by_id(&workspace_id);
    // Only now may this row's guard (if any) release — see its own doc
    // above: held from whichever arm took it (capsule end/stop) through this exact removal,
    // so neither a watchdog restart nor a waiting `agent.join` can act
    // on a row that is already gone.
    drop(destroy_guard);

    // Live-push to every connected frontend so the Sessions strip refreshes
    // without a manual workspace.list poll (mirror the create path). Clone
    // because slug/workspace_id are consumed by the bump + response below.
    let _ = ws_events.send(WorkspaceChanged {
        action: "destroyed".into(),
        slug: slug.clone(),
        workspace_id: workspace_id.clone(),
    });

    let rev = session
        .bump(
            "workspace.destroyed",
            json!({ "workspace_id": workspace_id, "slug": slug }),
        )
        .await;

    let res = WorkspaceDestroyRes {
        workspace_id,
        slug,
        label,
        tmux_killed: false,
        toml_removed,
        kept: None,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_DESTROY, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

#[cfg(test)]
#[path = "destroy_tests.rs"]
mod tests;
