//! The end API lifecycle calls: [`end_every`] ends rows and the drawer by one deadline without resuming any, and
//! [`forget`] drops the rows a past end could not remove.
//!
//! Each row is ended as `workspace.destroy` ends it, but with no resume first, retrying a refused end once a second;
//! any other runtime's row, and what could not be ended by the deadline, is counted, never guessed.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::time::Instant;

use crate::rows::run::end::CapsuleDestroyOutcome;
use crate::rows::{Workspace, WorkspaceChanged, Workspaces};

/// How often a refused end is tried again.
const RETRY_EVERY: Duration = Duration::from_secs(1);

/// What [`end_every`] did. `ended` and `forget` are both ended runs;
/// `forget` holds the rows whose registration files would not go, for the
/// next start to drop (#26). The anchor's row is kept on purpose and shows
/// in `ended`. The drawer has no workspace id, so it shows only in
/// `not_ended`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct EndReport {
    pub ended: Vec<String>,
    pub not_ended: u32,
    pub forget: Vec<String>,
}

/// One target's end.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// Ended; its registration is gone, or (the anchor) kept.
    Row(String),
    /// Ended, but its registration files would not go.
    Forget(String),
    Drawer,
    /// Already removed by another end: not ours to end, and not counted.
    Gone,
    /// Not confirmed ended by the deadline.
    Not,
}

/// End `rows`, and the drawer iff its pointer exists (#17), without
/// resuming any: concurrently at `resume_all`'s limit, each
/// retried while refused until `deadline`. An end still running at the
/// deadline is abandoned and counted not ended; it dies with the process.
/// A row of any runtime but capsule is counted not ended and never
/// touched: the product never runs `pkill` or `tmux kill-server`.
/// `reason` is the end-run reason each ended run records.
pub(crate) async fn end_every(
    rows: Vec<Arc<Workspace>>,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
    state_root: &Path,
    reason: &'static str,
    deadline: Instant,
) -> EndReport {
    let drawer = drawer_is_target(state_root);
    let anchor = workspaces.default_id();
    let limit = Arc::new(tokio::sync::Semaphore::new(crate::rows::run::resume::LANE_CONCURRENCY));
    let mut joins = Vec::with_capacity(rows.len() + 1);
    for ws in rows {
        let is_anchor = anchor.as_deref() == Some(ws.workspace_id.as_str());
        let (limit, workspaces, ws_events) = (limit.clone(), workspaces.clone(), ws_events.clone());
        joins.push((ws.workspace_id.clone(), tokio::spawn(async move {
            if ws.runtime != "capsule" {
                tracing::warn!(workspace_id = %ws.workspace_id, runtime = %ws.runtime, "{reason}: a row this end cannot end; it stays registered and running");
                return Ended::Not;
            }
            let Ok(Ok(_permit)) = tokio::time::timeout_at(deadline, limit.acquire_owned()).await else {
                return Ended::Not;
            };
            end_row(&ws, is_anchor, &workspaces, &ws_events, reason, deadline).await
        })));
    }
    if drawer {
        let (limit, state_root) = (limit.clone(), state_root.to_path_buf());
        joins.push(("the drawer".to_string(), tokio::spawn(async move {
            let Ok(Ok(_permit)) = tokio::time::timeout_at(deadline, limit.acquire_owned()).await else {
                return Ended::Not;
            };
            end_drawer(state_root, reason, deadline).await
        })));
    }
    join_by(deadline, joins).await
}

/// The rows of `ids` a past end ended but could not remove (#26): their registration files removed and the rows
/// unregistered, before any plan, so no start resumes one. Returns the ids whose registration still would not go.
pub(crate) fn forget(workspaces: &Workspaces, ids: &[String]) -> Vec<String> {
    let mut unremoved = Vec::new();
    for ws in workspaces.list().into_iter().filter(|ws| ids.contains(&ws.workspace_id)) {
        if !crate::rows::run::end::remove_row_files(&ws.slug) {
            tracing::error!(workspace_id = %ws.workspace_id, "a forgotten row's registration would not go; the next start drops it again");
            unremoved.push(ws.workspace_id.clone());
        }
        let _ = workspaces.remove_by_id(&ws.workspace_id);
    }
    unremoved
}

/// Every target's end, each awaited no later than `deadline`: one still
/// running then is abandoned and counted not ended.
async fn join_by(deadline: Instant, joins: Vec<(String, tokio::task::JoinHandle<Ended>)>) -> EndReport {
    let mut report = EndReport::default();
    for (target, j) in joins {
        match tokio::time::timeout_at(deadline, j).await {
            Ok(Ok(Ended::Row(id))) => report.ended.push(id),
            Ok(Ok(Ended::Forget(id))) => report.forget.push(id),
            Ok(Ok(Ended::Drawer | Ended::Gone)) => {}
            Ok(Ok(Ended::Not)) | Ok(Err(_)) => report.not_ended += 1,
            Err(_) => {
                tracing::warn!(%target, "row end: still running at the deadline; not ended");
                report.not_ended += 1;
            }
        }
    }
    report
}

/// The drawer is a target iff its pointer exists in the state root (#17).
fn drawer_is_target(state_root: &Path) -> bool {
    sot_log::supervisor::journal::pointer::pointer_path(state_root).is_file()
}

/// One row, as `workspace.destroy` ends it but with no resume first (#11).
/// The anchor's row is kept, as the default row's own end keeps it.
async fn end_row(
    ws: &Workspace,
    is_anchor: bool,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
    reason: &'static str,
    deadline: Instant,
) -> Ended {
    let (agent, agent_name) = (ws.agent(), ws.agent_name());
    let (agent_ref, name_ref) = (agent.as_str(), agent_name.as_str());
    let ended = retry_until(deadline, move || async move {
        let (outcome, held) = crate::rows::run::end::destroy_capsule_workspace(
            &ws.workspace_id,
            reason,
            agent_ref,
            name_ref,
            &ws.slug,
            &ws.project_root,
            workspaces,
            false,
        )
        .await;
        confirmed(outcome).map(|ours| ours.then_some(held))
    })
    .await;
    let held = match ended {
        Ok(Some(held)) => held,
        Ok(None) => {
            tracing::info!(workspace_id = %ws.workspace_id, "{reason}: already removed by another end");
            return Ended::Gone;
        }
        Err(detail) => {
            tracing::warn!(workspace_id = %ws.workspace_id, %detail, "{reason}: row not ended by the deadline; it stays registered and running");
            return Ended::Not;
        }
    };
    if is_anchor {
        let end =
            crate::rows::anchor::end_default_row_run(workspaces, ws_events, &ws.workspace_id, &ws.slug, &agent_name, true, held);
        if tokio::time::timeout_at(deadline, end).await.is_err() {
            tracing::warn!(workspace_id = %ws.workspace_id, "{reason}: the anchor's end_default_row_run was still running at the deadline; not ended");
            return Ended::Not;
        }
        return Ended::Row(ws.workspace_id.clone());
    }
    // The ended agent cannot run its own comm-leave; prune its registry
    // rows, as `workspace.destroy` does.
    let (reg_agent, reg_ws, reg_host) =
        (agent_name.clone(), ws.workspace_id.clone(), crate::rows::store::declared_host());
    let prune = tokio::task::spawn_blocking(move || {
        crate::comm::registry::registry::remove_comm_agents_for_workspace(&reg_agent, &reg_ws, &reg_host)
    });
    if tokio::time::timeout_at(deadline, prune).await.is_err() {
        tracing::warn!(workspace_id = %ws.workspace_id, "{reason}: the comm prune was still running at the deadline; not ended");
        return Ended::Not;
    }
    let slug = ws.slug.clone();
    let ended = forget_unless_removed(ws.workspace_id.clone(), deadline, || crate::rows::run::end::remove_row_files(&slug)).await;
    let _ = workspaces.remove_by_id(&ws.workspace_id);
    drop(held);
    let _ = ws_events.send(WorkspaceChanged {
        action: "destroyed".into(),
        slug: ws.slug.clone(),
        workspace_id: ws.workspace_id.clone(),
    });
    ended
}

/// The drawer: `end_run` on the state root itself, which stops its
/// authority after a confirmed end.
async fn end_drawer(state_root: PathBuf, reason: &'static str, deadline: Instant) -> Ended {
    // Canonicalized as `destroy_capsule_workspace` does, and for its reason.
    let (state_root, root_canonicalized) = match state_root.canonicalize() {
        Ok(canonical) => (canonical, true),
        Err(_) => (state_root, false),
    };
    let ended = retry_until(deadline, || {
        let state_root = state_root.clone();
        async move {
            match tokio::task::spawn_blocking(move || {
                crate::rows::run::end_run::end_run(&state_root, reason, root_canonicalized)
            })
            .await
            {
                Ok(Ok(o)) => confirmed(crate::rows::run::end::capsule_destroy_outcome_of(o)).map(|_| ()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(join_err) => Err(format!("end_run task panicked: {join_err}")),
            }
        }
    })
    .await;
    match ended {
        Ok(()) => Ended::Drawer,
        Err(detail) => {
            tracing::warn!(%detail, "{reason}: the drawer was not ended by the deadline; it stays running");
            Ended::Not
        }
    }
}

/// A confirmed end is done (`true`), and so is a row another end already
/// removed (`false`: not ours); a kept one is tried again.
fn confirmed(outcome: CapsuleDestroyOutcome) -> Result<bool, String> {
    match outcome {
        CapsuleDestroyOutcome::Removable(_) => Ok(true),
        CapsuleDestroyOutcome::AlreadyRemoved => Ok(false),
        CapsuleDestroyOutcome::Kept { detail } => Err(detail),
    }
}

/// An ended row's registration files, removed with retries until
/// `deadline`. Files that would not go put the id in `forget`, for the
/// next start to drop (#26).
async fn forget_unless_removed(id: String, deadline: Instant, mut remove: impl FnMut() -> bool) -> Ended {
    let removed = retry_until(deadline, || {
        let removed = remove();
        async move {
            if removed {
                Ok(())
            } else {
                Err("registration file not removed".to_string())
            }
        }
    })
    .await;
    match removed {
        Ok(()) => Ended::Row(id),
        Err(_) => Ended::Forget(id),
    }
}

/// Run `attempt` until it succeeds, once per second while it refuses,
/// until `deadline`. An attempt still running at the deadline is dropped;
/// whatever blocking work it started runs on, unwaited. `Err` carries the
/// last refusal.
async fn retry_until<T, F, Fut>(deadline: Instant, mut attempt: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let mut last = String::from("no attempt finished");
    loop {
        match tokio::time::timeout_at(deadline, attempt()).await {
            Ok(Ok(done)) => return Ok(done),
            Ok(Err(refused)) => last = refused,
            Err(_) => return Err(format!("still running at the deadline; last refusal: {last}")),
        }
        let next = Instant::now() + RETRY_EVERY;
        if next >= deadline {
            return Err(last);
        }
        tokio::time::sleep_until(next).await;
    }
}

#[cfg(test)]
#[path = "end_tests.rs"]
mod tests;
