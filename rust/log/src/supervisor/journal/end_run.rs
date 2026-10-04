//! EndRun over the voyage's own mgmt lane, and its reconciliation via the leg's durable marker.

use crate::store::voyage::{SEG_DIR, WRITER_LOCK};
use crate::supervisor::*;

// ---------------------------------------------------------------------
// EndRun over the voyage's own mgmt lane, and its reconciliation via the
// leg's own durable marker (ADR 0041)
// ---------------------------------------------------------------------

pub(in crate::supervisor) enum EndRunOutcome {
    Absent,
    Foreign,
    Pending,
    /// The challenge succeeded and the shutdown request reached the
    /// process. `Ended` covers BOTH a read-back ack AND an ack whose
    /// own write or read timed out/failed: every downstream caller already treated the two
    /// identically (`finish_end_run_with_process`'s own wait+marker
    /// sequence doesn't care whether the shutdown was CONFIRMED
    /// acknowledged or merely PROBABLY delivered — it proves the
    /// leg's actual outcome independently either way), so a distinct
    /// variant carried no decision either site ever made differently.
    Ended(Process),
}

/// ADR 0041 capability matrix's "healthy" row, and EndRun "invoked by the
/// authority on its own behalf": challenge afresh, retain the handle,
/// send `shutdown{reason}` on the SAME connection, and wait its ack.
pub(in crate::supervisor) fn end_run_over_mgmt_lane(h: &str, voyage_id: &str, reason: &str) -> crate::Result<EndRunOutcome> {
    let conn = match PlatformEndpoint::default().connect_voyage_unchallenged(h, voyage_id) {
        Ok(c) => c,
        // ADR 0043 decision 21: the ONE absence predicate, shared with
        // Windows -- see `TransportError::is_endpoint_absent`'s own doc.
        Err(e) if e.is_endpoint_absent() => {
            return Ok(EndRunOutcome::Absent);
        }
        Err(e) => return Err(e.into()),
    };
    let mut exchange = crate::identity::exchange::VoyageMgmtExchange::default();
    match PlatformEndpoint::default().challenge(&conn, &mut exchange, Instant::now() + END_RUN_CHALLENGE_BOUND) {
        ChallengeOutcome::Foreign => Ok(EndRunOutcome::Foreign),
        ChallengeOutcome::Undetermined => Ok(EndRunOutcome::Pending),
        ChallengeOutcome::Proven(process) => {
            let request = wire::encode_mgmt_request(&wire::MgmtRequest::Shutdown { reason: reason.to_string() })
                .map_err(|e| err_state(format!("encoding shutdown request: {e}")))?;
            // The exchange machinery already has a cancellable deadline
            // primitive (`crate::attach_client::supervisor_client::read_one_frame`,
            // just below, already uses it for its own read); reused
            // here rather than a second one, on the SAME "request write
            // 2s" per-op budget every other op already uses.
            let write_deadline = Instant::now() + END_RUN_WRITE_BOUND;
            let write_ok = crate::identity::deadline::run_with_deadline(write_deadline, || conn.cancel(), || conn.write_all(&request))
                .is_some_and(|r| r.is_ok());
            if !write_ok {
                return Ok(EndRunOutcome::Ended(process));
            }
            // The ack itself is read for wire-protocol hygiene (drain
            // what the peer sends), but its outcome no longer branches
            // anything — see `Ended`'s own doc above.
            let _ = crate::attach_client::supervisor_client::read_one_frame(&conn, Instant::now() + END_RUN_ACK_READ_BOUND);
            Ok(EndRunOutcome::Ended(process))
        }
    }
}

/// Whether the voyage's own mgmt-lane writer is still alive — the
/// question B3 requires answering BEFORE a marker check ever counts:
/// "recovery must first prove the writer is gone... only pipe-absent +
/// marker-present closes." A ONE-SHOT check (not a retry episode): this
/// asks "is anyone there RIGHT NOW", the same "connect 2s" per-op budget
/// every other op uses.
enum WriterLiveness {
    Alive,
    Absent,
    /// A wrong answer, or an OS-call failure — could not be determined
    /// either way. Treated the SAME as `Alive` by every caller: fail
    /// closed, never treat an ambiguous result as proof of absence.
    Ambiguous,
}

/// Pipe absence is NOT writer absence. The
/// capsule removes the pipe NAME before its final writes, seal, and
/// writer-fence release — so a
/// restarted supervisor that trusted pipe-silence alone could
/// `mark_closed`/`verify_voyage` an open chain tip while the original
/// writer still owns the fence and can append MORE history underneath
/// it. Once the pipe is proven absent, this additionally proves
/// `writer.lock` itself is free — the SAME bounded acquire-then-
/// immediately-release primitive `open_for_writing` uses
/// (`host::lock_writer`, its own ~250ms bounded retry) — before ever
/// trusting the silence. A held fence still means a live writer
/// (`Ambiguous`, fail-closed, exactly like every other undetermined
/// case here); only a genuinely free fence reaches `Absent`.
fn probe_writer_liveness(state_dir: &Path, voyage_id: &str) -> WriterLiveness {
    // The caller has no `h` of its own to pass -- derived here from the
    // `state_dir` this function already receives, rather than fanning the
    // parameter out through every caller above it.
    let h = crate::host::state_dir::state_dir_hash(state_dir);
    let conn = match PlatformEndpoint::default().connect_voyage_unchallenged(&h, voyage_id) {
        Ok(c) => c,
        // ADR 0043 decision 21: the ONE absence predicate, shared with
        // Windows -- see `TransportError::is_endpoint_absent`'s own doc.
        Err(e) if e.is_endpoint_absent() => {
            let root = voyage_root_path(state_dir, voyage_id);
            return match host::lock_writer(&root.join(WRITER_LOCK)) {
                Ok(lock) => {
                    drop(lock); // released immediately, per the module's own convention
                    WriterLiveness::Absent
                }
                Err(_) => WriterLiveness::Ambiguous,
            };
        }
        Err(_) => return WriterLiveness::Ambiguous,
    };
    let mut exchange = crate::identity::exchange::VoyageMgmtExchange::default();
    match PlatformEndpoint::default().challenge(&conn, &mut exchange, Instant::now() + LIVENESS_PROBE_BUDGET) {
        ChallengeOutcome::Proven(_) => WriterLiveness::Alive,
        ChallengeOutcome::Foreign | ChallengeOutcome::Undetermined => WriterLiveness::Ambiguous,
    }
}

/// What reconciling one `end_run` against the world concluded.
#[derive(Debug)]
pub(in crate::supervisor) enum EndRunReconciliation {
    /// Post-barrier (marker present), writer confirmed gone, verified.
    Ended,
    /// Pre-barrier: the writer is CONFIRMED gone (a wait()-confirmed
    /// exit, or [`probe_writer_liveness`] finding it `Absent`) but the
    /// marker never appeared — NOT ended: the hold releases, ordinary
    /// respawn/adopt logic decides, live or recovered, identically.
    /// `journal::finish` has ALREADY been called (`Failed{record_append}`).
    PreBarrierFailed,
    /// The writer is still `Alive`, or its
    /// liveness is `Ambiguous` — NEITHER `Ended` NOR `PreBarrierFailed`.
    /// The operation stays ACTIVE, untouched (no journal mutation at
    /// all) — never released, never respawned over. A live caller
    /// retries this same check in a bounded loop
    /// ([`spawn_end_run`]); a recovering caller simply leaves the entry
    /// `.active` for a LATER pass (this restart's own main loop already
    /// isn't reachable for an OLD entry — the NEXT restart, or a fresh
    /// `end_run`/`query` against this same id).
    PendingWriter,
}

/// The wait+marker+verify sequence for a LIVE caller holding a proven
/// process handle (an `Ended` outcome from [`end_run_over_mgmt_lane`]).
/// The wait result GATES `mark_closed`
/// (B4): only a CONFIRMED exit reaches the marker check. An unconfirmed
/// exit gets ONE hard-stop fallback (terminate + wait) before giving up
/// — B4's "an unresponsive mgmt lane has the hard-stop fallback instead
/// of leaking a live process" — never leaving a proven-but-unresponsive
/// process untracked. Never returns `PendingWriter`: a proven process
/// handle always resolves to a CONFIRMED exit or hard-stop before this
/// ever reaches [`reconcile_via_marker`], so writer-liveness ambiguity
/// (only possible with NO handle at all) cannot arise here.
/// `op_id` is `None` for the no-supervisor CLI path (`endrun_inner`),
/// which journals nothing at all — there is no `query{operation_id}`
/// caller for a durable record to ever serve, and a fixed placeholder id
/// would risk colliding with a REAL operation id a later supervised
/// session might actually use against the same state directory.
/// `on_closed` is `Option`, like [`finish_end_run_without_process`]'s own
/// parameter of the same name: `Some` for the live worker
/// ([`spawn_end_run`], via a connection it may owe a deferred reply to),
/// `None` for RECOVERY re-issuing this same call with no live connection
/// to signal through (`reconcile_journal_on_startup`) — the journal
/// itself carries the result for a later `query`.
pub(in crate::supervisor) fn finish_end_run_with_process(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    process: Process,
    on_closed: Option<&mpsc::Sender<EndingProgress>>,
) -> crate::Result<EndRunReconciliation> {
    let confirmed_exit = match process.wait(SUPPORTED_HISTORY_BOUND + KILL_WAIT_BOUND) {
        Ok(true) => true,
        Ok(false) | Err(_) => {
            let _ = process.terminate();
            matches!(process.wait(KILL_WAIT_BOUND), Ok(true))
        }
    };
    // Single-owner reaping every Unix: whichever `wait`
    // call above actually confirmed the exit (the graceful one, or the
    // terminate-then-wait fallback), `process` is never read again past
    // this point — reap it now, explicitly (see `ChallengedProcess::reap`'s
    // own doc). A Windows process HANDLE has no zombie/reap concept at
    // all — `Drop`'s own `CloseHandle` is the whole cleanup there, so the
    // Windows `Process` type gains nothing from a call here.
    //
    // `cfg(unix)`, NOT `cfg(target_os = "linux")`: the concept this gate
    // names is "this OS has zombies", which is every Unix, and spelling
    // it `linux` made the macOS build compile the call away to NOTHING —
    // a supervisor that never reaps, leaking a zombie per leg, with no
    // compiler and no CI able to say so. The module gate above is
    // `any(windows, linux, macos)`, so `unix` here is exactly those two
    // Unixes and both have a real `reap`.
    #[cfg(unix)]
    if confirmed_exit {
        process.reap();
    }
    if !confirmed_exit {
        let detail = bounded_detail("the leg's process did not exit even after a hard stop");
        if let Some(op_id) = op_id {
            journal::finish(state_dir, op_id, &journal::TerminalRecord::Failed { detail: detail.clone() })?;
        }
        return Err(err_state(detail));
    }
    reconcile_via_marker(state_dir, op_id, voyage_id, epoch, on_closed)
}

/// As [`finish_end_run_with_process`], but for a caller with NO proven
/// handle at all (recovery, or the live path's Absent/Foreign/Pending/
/// error outcomes — B4: "Foreign/Pending/mgmt errors during EndRun still
/// run marker reconciliation"). Proves the writer is gone FIRST (B3),
/// and can return `PendingWriter` (never releasing the hold)
/// or, when the writer IS proven gone and `on_closed` is given, still
/// signal `RecordClosed` through it exactly as the WITH-process path
/// does.
pub(in crate::supervisor) fn finish_end_run_without_process(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    on_closed: Option<&mpsc::Sender<EndingProgress>>,
) -> crate::Result<EndRunReconciliation> {
    match probe_writer_liveness(state_dir, voyage_id) {
        WriterLiveness::Alive | WriterLiveness::Ambiguous => Ok(EndRunReconciliation::PendingWriter),
        WriterLiveness::Absent => reconcile_via_marker(state_dir, op_id, voyage_id, epoch, on_closed),
    }
}

/// The shared marker-check-then-verify tail, reached only once the
/// writer is KNOWN gone (by a confirmed wait, or by
/// [`probe_writer_liveness`] finding it absent). `on_closed`, if given,
/// is signalled the moment `mark_closed` succeeds — the deferred-reply
/// correlation B3 requires (`None` for the recovery path, which has no
/// live connection to reply to). Every `journal::finish`/`mark_closed`
/// call is skipped when `op_id` is `None` (the CLI path) — the
/// RECONCILIATION outcome is computed identically either way. Never
/// returns `PendingWriter` — that outcome belongs to the writer-liveness
/// check ABOVE this function, never to what happens once it is gone.
fn reconcile_via_marker(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    on_closed: Option<&mpsc::Sender<EndingProgress>>,
) -> crate::Result<EndRunReconciliation> {
    let seg_dir = voyage_root_path(state_dir, voyage_id).join(SEG_DIR);
    let epoch = match epoch {
        Some(e) => e,
        None => match recovery::latest_leg_state(&seg_dir).map_err(crate::Error::Io)? {
            LatestLegState::Sealed { epoch } | LatestLegState::Unsealed { epoch } => epoch,
            LatestLegState::NoLeg => {
                if let Some(op_id) = op_id {
                    journal::finish(
                        state_dir,
                        op_id,
                        &journal::TerminalRecord::Failed { detail: bounded_detail("no leg exists for this voyage") },
                    )?;
                }
                return Ok(EndRunReconciliation::PreBarrierFailed);
            }
        },
    };
    let marked = verify::leg_carries_run_end_marker(&seg_dir, voyage_id, epoch)?;
    if !marked {
        if let Some(op_id) = op_id {
            journal::finish(state_dir, op_id, &journal::TerminalRecord::Failed { detail: bounded_detail("record_append") })?;
        }
        return Ok(EndRunReconciliation::PreBarrierFailed);
    }
    if let Some(op_id) = op_id {
        journal::mark_closed(state_dir, op_id)?;
    }
    if let Some(tx) = on_closed {
        let _ = tx.send(EndingProgress::RecordClosed);
    }
    let root = voyage_root_path(state_dir, voyage_id);
    match verify::verify_voyage(&root, voyage_id) {
        Ok(()) => {
            if let Some(op_id) = op_id {
                journal::finish(state_dir, op_id, &journal::TerminalRecord::RecordVerified)?;
            }
            Ok(EndRunReconciliation::Ended)
        }
        Err(e) => {
            let detail = bounded_detail(format!("verify_voyage: {e}"));
            if let Some(op_id) = op_id {
                journal::finish(state_dir, op_id, &journal::TerminalRecord::Failed { detail: detail.clone() })?;
            }
            Err(err_state(detail)) // sticky Terminal — B4
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer whose liveness cannot be
    /// disproven (here: `writer.lock` genuinely HELD, with no pipe
    /// bound at all for this voyage — the same "pipe absent" state a
    /// real post-teardown window produces) must be `PendingWriter`,
    /// never `PreBarrierFailed` — the operation stays untouched, never
    /// released for a respawn to run over a writer that might still be
    /// alive.
    #[test]
    fn a_held_writer_lock_with_no_pipe_is_pending_writer_never_pre_barrier_failed() {
        let dir = tempfile::tempdir().unwrap();
        let voyage_id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let root = voyage_root_path(dir.path(), &voyage_id);
        let _held = host::lock_writer(&root.join("writer.lock")).unwrap();

        let (tx, _rx) = mpsc::channel();
        let result = finish_end_run_without_process(dir.path(), Some("op-1"), &voyage_id, None, Some(&tx));
        assert!(matches!(result, Ok(EndRunReconciliation::PendingWriter)), "expected PendingWriter, got {result:?}");
        // No journal entry was ever begun for "op-1" in this test, and
        // PendingWriter must not have created one either -- there is
        // nothing to release or respawn over.
        assert!(journal::read_active(dir.path(), "op-1").unwrap().is_none());
    }
}
