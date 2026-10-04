//! Journal recovery, run first on startup, before pointer discovery.

use crate::supervisor::*;

// ---------------------------------------------------------------------
// Journal recovery, run FIRST — before pointer discovery (B1)
// ---------------------------------------------------------------------

pub(in crate::supervisor) struct ReconciliationSummary {
    /// Voyages whose recovered `end_run` reached `RecordVerified` this
    /// pass — the caller checks membership AFTER discovering the
    /// (now-reconciled) current voyage, never before (B1).
    pub(in crate::supervisor) ended_voyages: std::collections::HashSet<String>,
}

/// Reconciles every ACTIVE journal entry against the world — voyage
/// agnostic, keyed off nothing but `state_dir` and each entry's own
/// recorded voyage. A post-barrier verification failure propagates as
/// `Err` (STICKY `Terminal` for the caller, B4), aborting the sweep
/// immediately: the whole authority is going Terminal regardless, so
/// there is no point reconciling anything else.
pub(in crate::supervisor) fn reconcile_journal_on_startup(state_dir: &Path) -> crate::Result<ReconciliationSummary> {
    let mut ended_voyages = std::collections::HashSet::new();
    for op_id in journal::active_operations(state_dir)? {
        let Some(active) = journal::read_active(state_dir, &op_id)? else { continue };
        match &active.op {
            journal::ActiveOp::EndRun { voyage, epoch } => {
                // Re-issue the `end_run` exactly as the live worker
                // would, via the SAME shared sequence
                // ([`reissue_and_reconcile_end_run`]) — a worker killed
                // after the journal record was accepted but BEFORE it
                // ever called `end_run_over_mgmt_lane` leaves the
                // capsule never asked to end; jumping straight to
                // `retry_until_writer_resolved`
                // only waits for a writer that was never told to
                // go away and so never resolves. `on_closed: None` —
                // recovery has no live connection to signal through;
                // the journal itself carries the result for a later
                // `query`. Retried right
                // here, in THIS worker (already bounded from OUTSIDE by
                // RECOVERY_WATCHDOG, exactly as `spawn_end_run`'s own
                // retry is bounded by ENDING_WATCHDOG).
                match reissue_and_reconcile_end_run(
                    state_dir,
                    Some(&op_id),
                    voyage,
                    *epoch,
                    RECOVERY_END_RUN_REASON,
                    None,
                )? {
                    EndRunReconciliation::Ended => {
                        ended_voyages.insert(voyage.clone());
                    }
                    EndRunReconciliation::PreBarrierFailed => {}
                    EndRunReconciliation::PendingWriter => {
                        unreachable!("retry_until_writer_resolved only returns once no longer PendingWriter")
                    }
                }
            }
            journal::ActiveOp::Reset { old_voyage, new_voyage, aside } => {
                reconcile_reset(state_dir, &op_id, new_voyage, old_voyage.as_deref(), aside.as_deref())?;
            }
            journal::ActiveOp::Stop => {
                // A Stop's effect is process
                // exit; a crash after admission means the effect
                // already happened. Finish it as the terminal fact it
                // always was — loud (`?`) if that write itself fails —
                // and then fall through to ordinary startup exactly
                // like any other reconciled entry: this authority does
                // NOT enter any special state and does NOT exit before
                // pointer discovery on account of it. A fresh
                // `supervise` invocation is a FRESH operator intent; an
                // old Stop stays answerable via `query` for whoever
                // asked, but honoring it against THIS run would make
                // the authority unstartable — see the module doc's own
                // "Stop no longer owns a Lifecycle state" section.
                journal::finish(state_dir, &op_id, &journal::TerminalRecord::Stopping)?;
            }
        }
    }
    Ok(ReconciliationSummary { ended_voyages })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A crashed authority's own
    /// admitted-but-unfinished `Stop` is FINISHED as terminal `Stopping`
    /// by the next restart's recovery pass — loud on failure, via the
    /// bare `?` `reconcile_journal_on_startup` already propagates.
    #[test]
    fn recovery_finishes_a_crashed_stop_as_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let record = journal::ActiveRecord {
            operation_id: "stop-1".into(),
            digest: digest_of(&SupervisorOp::Stop).unwrap(),
            op: journal::ActiveOp::Stop,
        };
        journal::begin(dir.path(), "stop-1", &record).unwrap();
        reconcile_journal_on_startup(dir.path()).unwrap();
        assert_eq!(journal::read_terminal(dir.path(), "stop-1").unwrap(), Some(journal::TerminalRecord::Stopping));
    }
}
