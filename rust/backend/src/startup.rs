//! startup.rs — a start's decision from `held.json`, and acting on it.
//!
//! [`begin`] reads the record, plans from it and this boot alone
//! ([`lease::startup_plan`]), builds the daemon's [`Leases`] and acts on
//! the plan. Every plan but Cleanup resumes the rows at once; recorded
//! holders or a handover also arm their persisted deadline, which the
//! ticker turns into a shutdown if no lease arrives. Nothing is
//! held back. Cleanup ends every row through the shutdown's own end and
//! resumes none; the daemon stays up with zero sessions.

use std::path::PathBuf;
use std::sync::Arc;

use sot_protocol::ops::lease as bounds;
use tokio::sync::{broadcast, mpsc};

use crate::lease::{self, Leases, StartEvent, StartPlan};
use crate::workspaces::{WorkspaceChanged, Workspaces};

/// The daemon's leases, built from the record before any connection is
/// accepted, so no grant can rewrite the record from a state that lacks
/// it. With no state root there is no record: no plan, no resume, as the
/// resume skip has always been.
pub(crate) fn begin(
    state_root: Option<PathBuf>,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
) -> Arc<Leases> {
    let own_boot = sot_log::challenge::boot_identity();
    let now_ms = lease::now_ms();
    if let Err(e) = &own_boot {
        tracing::warn!("boot identity unknown, so no lease can be granted: {e}");
    }
    let Some(state_root) = state_root else {
        tracing::warn!(
            "capsule workspace resume-scan skipped: could not resolve this machine's state root \
             ({} unset)",
            crate::capsule_workspace::STATE_ROOT_HINT
        );
        let (leases, starts) = Leases::new(own_boot.ok(), None, now_ms);
        log_starts(starts);
        return Arc::new(leases);
    };
    let path = state_root.join(bounds::HELD_RECORD_FILE);
    let read = lease::read_record(&path);
    if let Err(e) = &read {
        tracing::warn!("held record unreadable: {e}");
    }
    let plan = lease::startup_plan(&read, own_boot.as_deref().map_err(|_| ()), now_ms);
    let (leases, starts) = Leases::new(own_boot.ok(), Some(path), now_ms);
    let leases = Arc::new(leases);
    log_starts(starts);
    tracing::info!(?plan, "start plan from the held record");
    match plan {
        StartPlan::Resume => {
            tokio::spawn(crate::capsule_workspace::resume_all(state_root, workspaces.clone()));
        }
        StartPlan::Pending { until_ms } => {
            tokio::spawn(crate::capsule_workspace::resume_all(state_root, workspaces.clone()));
            if let Err(e) = leases.install_pending(until_ms) {
                tracing::error!("the pending start's deadline was not written: {e}");
            }
        }
        StartPlan::Cleanup => {
            tokio::spawn(cleanup(leases.clone(), state_root, workspaces.clone(), ws_events.clone()));
        }
    }
    leases
}

/// A startup Cleanup: every row ended without a resume, by the shutdown's
/// bound, then its report written as the record. Ended rows are forgotten
/// by the end itself.
async fn cleanup(
    leases: Arc<Leases>,
    state_root: PathBuf,
    workspaces: Workspaces,
    ws_events: broadcast::Sender<WorkspaceChanged>,
) {
    let deadline = tokio::time::Instant::now() + bounds::SHUTDOWN_BOUND;
    let report = crate::shutdown::end_rows(&workspaces, &ws_events, &state_root, deadline).await;
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

/// Every row was resumed at start, so a lease only ends the
/// wait; it is logged.
fn log_starts(mut starts: mpsc::UnboundedReceiver<StartEvent>) {
    tokio::spawn(async move {
        while let Some(event) = starts.recv().await {
            tracing::info!(?event, "a window completed the pending start");
        }
    });
}
