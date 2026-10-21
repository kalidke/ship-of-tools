//! startup.rs — a start's decision from `held.json`, and acting on it.
//!
//! [`begin`] reads the record, drops the rows it says to forget, plans
//! from it and this boot alone ([`lease::startup_plan`]), builds the
//! daemon's [`Leases`] and acts on the plan. Every plan but Cleanup resumes the rows at once; recorded
//! holders or a handover also arm their persisted deadline, which the
//! ticker turns into a shutdown if no lease arrives. Nothing is
//! held back. Cleanup ends every row through rows' end API, as the shutdown does, and
//! resumes none, and refuses a create of those rows' labels until it
//! ends; the daemon stays up with zero sessions.

use std::path::PathBuf;
use std::sync::Arc;

use sot_protocol::ops::lease as bounds;
use tokio::sync::broadcast;

use crate::lifecycle::lease::{self, Leases, StartPlan};
use crate::rows::{Workspace, WorkspaceChanged, Workspaces};

/// The daemon's leases, built from the record before any connection is
/// accepted, so no grant can rewrite the record from a state that lacks
/// it. With no state root there is no record: no plan, no resume, as the
/// resume skip has always been.
pub(crate) fn begin(
    state_root: Option<PathBuf>,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
) -> Arc<Leases> {
    let own_boot = sot_log::identity::challenge::boot_identity();
    let now_ms = lease::now_ms();
    if let Err(e) = &own_boot {
        tracing::warn!("boot identity unknown, so no lease can be granted: {e}");
    }
    let Some(state_root) = state_root else {
        tracing::warn!(
            "capsule workspace resume-scan skipped: could not resolve this machine's state root \
             ({} unset)",
            crate::rows::STATE_ROOT_HINT
        );
        return Arc::new(Leases::new(own_boot.ok(), None, None, false));
    };
    let path = state_root.join(bounds::HELD_RECORD_FILE);
    let read = lease::read_record(&path);
    if let Err(e) = &read {
        tracing::warn!("held record unreadable: {e}");
    }
    let unremoved = match &read {
        Ok(Some(rec)) => crate::rows::end::forget(workspaces, &rec.forget),
        _ => Vec::new(),
    };
    let plan = lease::startup_plan(&read, own_boot.as_deref().map_err(|_| ()), now_ms);
    let loaded = read.as_ref().ok().and_then(Option::as_ref);
    let leases = Leases::new(own_boot.ok(), Some(path), loaded, plan == StartPlan::Cleanup);
    leases.keep_unremoved(&unremoved);
    let leases = Arc::new(leases);
    tracing::info!(?plan, "start plan from the held record");
    match plan {
        StartPlan::Resume => {
            tokio::spawn(crate::rows::resume_all(state_root, workspaces.clone()));
        }
        StartPlan::Pending { until_ms } => {
            tokio::spawn(crate::rows::resume_all(state_root, workspaces.clone()));
            if let Err(e) = leases.install_pending(until_ms) {
                tracing::error!("the pending start's deadline was not written: {e}");
            }
        }
        StartPlan::Cleanup => {
            // The rows registered now, before any listener binds, and their
            // slugs claimed as a create claims one, until the Cleanup ends: a
            // same-label create would keep a listed row's id, so it is refused
            // meanwhile, and a row a window creates is never this Cleanup's to end.
            let rows = workspaces.list();
            let claims: Vec<_> = rows.iter().filter_map(|ws| workspaces.claim_create(&ws.slug)).collect();
            let run = cleanup(leases.clone(), rows, state_root, workspaces.clone(), ws_events.clone());
            tokio::spawn(async move {
                run.await;
                drop(claims);
            });
        }
    }
    leases
}

/// A startup Cleanup: `rows` ended without a resume, by the shutdown's
/// bound, then its report written as the record. Ended rows are forgotten
/// by the end itself.
async fn cleanup(
    leases: Arc<Leases>,
    rows: Vec<Arc<Workspace>>,
    state_root: PathBuf,
    workspaces: Workspaces,
    ws_events: broadcast::Sender<WorkspaceChanged>,
) {
    let deadline = tokio::time::Instant::now() + crate::lifecycle::shutdown::shutdown_bound();
    let report = crate::rows::end::end_every(rows, &workspaces, &ws_events, &state_root, crate::lifecycle::shutdown::REASON, deadline).await;
    tracing::info!(
        ended = report.ended.len(),
        not_ended = report.not_ended,
        forget = ?report.forget,
        "startup cleanup: rows ended without a resume"
    );
    if let Err(e) = leases.finish_cleanup(report.not_ended, report.forget) {
        tracing::error!("startup cleanup's record was not written: {e}");
    }
}

#[cfg(all(test, unix))]
#[path = "startup_tests.rs"]
mod tests;
