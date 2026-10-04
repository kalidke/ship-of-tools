//! startup.rs — a start's decision from `held.json`, and acting on it.
//!
//! [`begin`] reads the record, drops the rows it says to forget, plans
//! from it and this boot alone ([`lease::startup_plan`]), builds the
//! daemon's [`Leases`] and acts on the plan. Every plan but Cleanup resumes the rows at once; recorded
//! holders or a handover also arm their persisted deadline, which the
//! ticker turns into a shutdown if no lease arrives. Nothing is
//! held back. Cleanup ends every row through the shutdown's own end and
//! resumes none; the daemon stays up with zero sessions.

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
            crate::rows::spawn::state_root::STATE_ROOT_HINT
        );
        return Arc::new(Leases::new(own_boot.ok(), None, None, false));
    };
    let path = state_root.join(bounds::HELD_RECORD_FILE);
    let read = lease::read_record(&path);
    if let Err(e) = &read {
        tracing::warn!("held record unreadable: {e}");
    }
    let unremoved = match &read {
        Ok(Some(rec)) => forget_rows(workspaces, &rec.forget),
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
            tokio::spawn(crate::rows::run::resume::resume_all(state_root, workspaces.clone()));
        }
        StartPlan::Pending { until_ms } => {
            tokio::spawn(crate::rows::run::resume::resume_all(state_root, workspaces.clone()));
            if let Err(e) = leases.install_pending(until_ms) {
                tracing::error!("the pending start's deadline was not written: {e}");
            }
        }
        StartPlan::Cleanup => {
            // The rows registered now, before any listener binds: a row a
            // window creates later is never this Cleanup's to end.
            let rows = workspaces.list();
            tokio::spawn(cleanup(leases.clone(), rows, state_root, workspaces.clone(), ws_events.clone()));
        }
    }
    leases
}

/// The record's `forget`: rows a past end ended whose registration would
/// not go (#26). Unregistered before any plan, so no start resumes one.
/// Returns the ids whose registration still would not go.
fn forget_rows(workspaces: &Workspaces, ids: &[String]) -> Vec<String> {
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
    let report = crate::lifecycle::shutdown::end_rows(rows, &workspaces, &ws_events, &state_root, deadline).await;
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
mod tests {
    use super::*;
    use std::ffi::OsString;

    use sot_log::identity::challenge::{PeerAuthOutcome, PeerAuthenticated};
    use sot_protocol::ops::{FeLeaseReq, LeaseOutcome};

    use crate::lifecycle::lease::HeldRecord;

    /// The config env this test points at a tempdir, put back on drop.
    struct EnvBack(Vec<(&'static str, Option<OsString>)>);

    impl Drop for EnvBack {
        fn drop(&mut self) {
            for (key, val) in &self.0 {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[tokio::test]
    async fn failed_forget_removal_is_kept_for_the_next_start() {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _back = EnvBack(["XDG_CONFIG_HOME", "SOT_SELF_HOST"].map(|k| (k, std::env::var_os(k))).to_vec());
        let config = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", config.path());
        std::env::set_var("SOT_SELF_HOST", "forget-test");

        let workspaces = Workspaces::new();
        let row = |label: &str| {
            let root = PathBuf::from("/p").join(label);
            workspaces.insert(Workspace::from_label(label, root, false, "none".into(), String::new(), String::new()))
        };
        let stuck = row("stuck");
        let gone = row("gone");
        // A directory where the registration file goes: it will not go.
        std::fs::create_dir_all(crate::rows::store::toml_path_for(&stuck.slug)).unwrap();

        let own = sot_log::identity::challenge::boot_identity().expect("this host's boot");
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join(bounds::HELD_RECORD_FILE);
        let rec = HeldRecord {
            v: 1,
            boot: own.clone(),
            holders: vec![],
            handover_until_ms: None,
            closing: false,
            not_ended: 0,
            forget: vec![stuck.workspace_id.clone(), gone.workspace_id.clone()],
        };
        lease::write_or_delete(&path, &rec).unwrap();

        let (ws_events, _rx) = broadcast::channel(4);
        let leases = begin(Some(state.path().to_path_buf()), &workspaces, &ws_events);
        let req = FeLeaseReq { boot: own, pid: 1, created: 7001, token: None };
        let peer = PeerAuthOutcome::Authenticated(PeerAuthenticated { pid: 1, created: 7001 });
        assert_eq!(leases.grant(&req, &peer).0, LeaseOutcome::Granted);
        assert_eq!(
            lease::read_record(&path).unwrap().map(|r| r.forget),
            Some(vec![stuck.workspace_id.clone()]),
            "a forgotten row whose registration would not go was dropped from the record"
        );
    }
}
