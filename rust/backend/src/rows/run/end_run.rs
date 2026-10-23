//! `end_run`: ending a capsule row's run, with the destroy proof (the fence free and nothing answering the row socket) before a row is reported ended.

use std::path::Path;
#[cfg(not(target_os = "linux"))]
use std::path::PathBuf;

/// Outcome of [`end_run`] — the daemon's own portable
/// vocabulary over `sot_log::attach_client::supervisor_client::EndRunOutcome` (never
/// that raw, platform-specific type crossing into `rows/run/end.rs`). Defined
/// here, apart from the platform-specific spawn code, so `rows/run/end.rs`'s outcome→response
/// mapping stays plain and unit-testable on every platform.
#[derive(Debug, Clone)]
pub enum EndRunOutcome {
    /// The run ended and its requested end is recorded: the supervisor answered `ended`.
    Ended,
    /// The supervisor was ALREADY resting in `EndedNoRespawn`: a prior end landed here, so this call asked for none and
    /// must not be reported as `Ended`.
    AlreadyEnded,
    /// The authority had already reached `Terminal` before this call ever reached it: its own failure to serve the
    /// row (its folders, its lane, the run's failure), or the watchdog's restart budget exhausted over an agent that
    /// can never launch (e.g. `claude` missing from PATH). A `Terminal` authority admits no fresh `EndRun`, so this
    /// sends `stop` instead (admitted unconditionally, whatever the phase) and waits for its confirmed exit. Without
    /// this arm the row was UNENDABLE: `workspace.destroy` kept reporting `NotEnded` forever, because nothing ever told
    /// the stuck authority to stop.
    ///
    /// `Terminal` alone does NOT prove the agent died (Codex review,
    /// 2026-09-11: watchdog restart-budget exhaustion, a failed
    /// adoption, or a kill/wait failure can all reach `Terminal` with an
    /// agent still running) — this variant is reported ONLY once the
    /// SAME [`absence_proof`] `Unheld` uses has independently confirmed
    /// the fence free and nothing answering the row socket; a socket
    /// that still answers is reported (`end_run` keeps the row), never
    /// silently orphaned.
    Terminal,
    /// `end_run` reported the operation failed, was refused, or its
    /// outcome is unknown — no confirmed end in any case.
    NotEnded(String),
    /// The lane itself was unreachable (the `query_status` round trip
    /// failed — no listener answered at all, not merely a phase this
    /// call disagreed with) AND BOTH halves of decision 33's destroy
    /// proof came back absent: a bounded, non-blocking attempt to take
    /// `supervisor.lock` on the same state dir succeeded — nobody holds
    /// the AUTHORITY over this row (the kernel released the fence the
    /// instant its last holder died — `sot_log::supervisor::journal::fence`) — AND
    /// [`row_socket_silent`] found nothing answering the row's socket
    /// (the agent ends with its supervisor). No authority AND nothing
    /// answering is safe to treat as `Removable`, same as `Terminal`.
    /// Fence free but a socket still answering is NOT this variant — it
    /// is reported instead of silently orphaned (see `end_run`'s own
    /// doc). Distinct from `NotEnded`: that
    /// variant means the lane DID answer and refused/failed the request
    /// — a live, responsive holder, never fabricated as ended. Field
    /// defect closed (v0.6.0-rc.12): a supervisor that died out from
    /// under a row (a daemon-pair converge that ended the old build)
    /// left the row permanently `Kept`/unendable, because `query_status`
    /// failing was the ONLY signal this function ever consulted.
    Unheld,
    /// The state directory itself does not exist AND `query_status`'s
    /// connect failure conclusively proves no supervisor answers this
    /// row's lane (`is_definitely_orphaned` — decision 27's own
    /// "no listener at all" classification, never a mere timeout or a
    /// foreign/undetermined challenge). Distinct from `Unheld`: that
    /// variant proves absence by taking the fence; it cannot even be
    /// attempted here because it lives INSIDE the missing directory
    /// (`fence.rs`) — so there is nowhere left for it to exist, let
    /// alone be held. This row
    /// has no durable record and no supervisor that could be alive under
    /// this daemon's state root — safe to remove, same as `Unheld`, but
    /// reported under its own name (`orphan_removed`) rather than folded
    /// into "no supervisor held the row", which would misstate that a
    /// row here ever really ran under this daemon.
    Orphaned,
}

/// `workspace.delete` on a capsule workspace (and the default row's
/// end-run path): send `row.end_run {run, reason}` on the row socket,
/// reporting the outcome via [`super::EndRunOutcome`] (see its own
/// variant docs for the Starting/AlreadyEnded/Terminal honesty
/// rules), THEN `stop` the authority once confirmed there is no more
/// leg to run — including a `Terminal` authority, which has no leg
/// to end but still needs `stop` to actually go away (see
/// [`super::EndRunOutcome::Terminal`]'s own doc: without this arm a
/// capsule row whose agent argv can never launch cycled
/// Starting -> Terminal forever and was never endable from the UI).
/// `stop` now WAITS for confirmed process exit; a failure there is
/// only a logged warning, never a destroy failure — the outcome
/// stays the confirmed one (`stop` only ends the AUTHORITY, never
/// the capsule LEG, ADR 0041 adoption). The state directory is NEVER
/// deleted here. BLOCKING — callers run it via `spawn_blocking`.
/// `root_canonicalized` (Fable review, safety): true only when the
/// caller (`destroy_capsule_workspace`) successfully canonicalized
/// the STATE ROOT before building `state_dir` — see that call site's
/// own doc for why an un-canonicalized root makes the orphan proof
/// below unsafe (a symlinked root can make this call dial a
/// different lane address than a live supervisor, spawned while its
/// own `state_dir` existed, actually bound). `false` disables the
/// orphan proof outright and keeps today's unconditional
/// `state_dir_missing` refusal, regardless of what `query_status`'s
/// connect returned.
#[allow(clippy::too_many_lines, reason = "asks a capsule to end its run and judges the answer; predates the 100-line limit")]
pub fn end_run(
    state_dir: &Path,
    reason: &str,
    root_canonicalized: bool,
) -> std::io::Result<super::EndRunOutcome> {
    use super::EndRunOutcome as R;
    use sot_log::attach_client::supervisor_client::EndRunOutcome as O;
    use sot_log::SupervisorPhase;

    // A4b: the row's remembered scopes (`row_scope::SCOPES_FILE`)
    // are ended after the graceful end when a supervisor answers, and
    // before `Unheld` when none does, so no retry and no restarted
    // daemon counts the row ended while a scope may hold processes.
    #[cfg(target_os = "linux")]
    let (root, own) = (super::row_scope::root(), super::row_scope::own_rel().unwrap_or_default());
    #[cfg(not(target_os = "linux"))]
    let (root, own) = (PathBuf::new(), String::new());

    let (status, scope) = match sot_log::attach_client::supervisor_client::query_status(state_dir) {
        Ok((status, process)) => {
            // A4b: the challenged supervisor's own scope, captured
            // and listed durably before the end that makes it exit; a
            // failed write kills nothing.
            #[cfg(target_os = "linux")]
            let scope = match super::row_scope::capture(&root, state_dir, process.pid()) {
                Ok(scope) => scope,
                Err(d) => return Ok(R::NotEnded(d)),
            };
            #[cfg(not(target_os = "linux"))]
            let scope: Option<String> = {
                let _ = process;
                None
            };
            (status, scope)
        }
        Err(e) => {
            // Recoverability (ADR 0043 decision 33): a row is
            // removed only after a confirmed end or a PROVEN absence
            // of BOTH the fence and the row socket — a
            // missing state dir proves neither BY ITSELF, and fence
            // creation there fails outright (`CREATE_NEW` needs the
            // directory), so `absence_proof` below cannot even be
            // attempted. It is checked and reported FIRST, distinct
            // from every other "lane unreachable" case — never a
            // licence to recreate anything (`row_socket_silent`'s own
            // caller, `destroy_capsule_workspace`, never does).
            //
            // STAT STRICTLY (Fable review): `fs::metadata`, not
            // `Path::is_dir()` — that helper swallows every error
            // (permission denied, a stale network-mount handle, any
            // other I/O failure) into a bare `false`, which used to
            // read identically to "genuinely absent". Only
            // `ErrorKind::NotFound` means missing; anything else
            // keeps refusing with ITS OWN detail, never folded into
            // `state_dir_missing` and never treated as grounds for
            // the orphan proof — a network mount hiccup must never
            // remove a row whose record exists.
            match std::fs::metadata(state_dir) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(std::io::Error::other(
                        "state dir path exists but is not a directory",
                    ));
                }
                Err(stat_err) if stat_err.kind() == std::io::ErrorKind::NotFound => {
                    // A missing directory is not automatically a dead
                    // end, though: `supervisor.lock` and every
                    // the fence live INSIDE `state_dir`
                    // (`fence.rs`), so if the
                    // directory is gone neither lock can possibly be
                    // held BY THIS ROW anywhere else — the one thing
                    // that could still be alive is a supervisor
                    // process answering THIS row's lane, which is
                    // addressed by a hash of the CANONICAL path
                    // (`state_dir_hash`) in the runtime dir, not a
                    // file under `state_dir` — so it stays reachable
                    // regardless of whether the directory exists,
                    // PROVIDED the caller resolved that canonical
                    // form (`root_canonicalized`). `query_status`'s
                    // own connect already tested exactly that, and
                    // `e`'s shape says whether it was conclusive:
                    // `is_definitely_orphaned` accepts only decision
                    // 27's own "no listener at all" classification
                    // (`TransportError::is_endpoint_absent`: connect
                    // refused or nothing there), never a timeout or a
                    // foreign/undetermined challenge, either of which
                    // means SOMETHING answered and the row must keep
                    // refusing. Proven absent -> `Orphaned` (no
                    // durable record, no reachable authority, no
                    // possible lock holder); otherwise the original
                    // unchanged refusal.
                    return if root_canonicalized && is_definitely_orphaned(&e) {
                        Ok(R::Orphaned)
                    } else {
                        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "state_dir_missing"))
                    };
                }
                Err(stat_err) => {
                    return Err(std::io::Error::other(format!(
                        "state dir stat failed (not proof of absence): {stat_err}"
                    )));
                }
            }
            // Unreachable is not the same claim as "not running" —
            // the caller must keep refusing a live-but-unresponsive
            // lane, never fabricate "ended" for one (see
            // `destroy_capsule_workspace`'s own doc). [`absence_proof`]
            // settles it independently of this IPC round trip; a
            // fence-stage failure keeps the ORIGINAL "lane
            // unreachable" text (`e`), unchanged.
            return match absence_proof(state_dir) {
                Ok(true) => {
                    // A4b: scopes an earlier end listed are proven
                    // empty before the row is counted ended.
                    #[cfg(target_os = "linux")]
                    if let Err(d) =
                        super::row_scope::end(&root, &own, state_dir, None, super::row_scope::SCOPE_EMPTY_BOUND)
                    {
                        return Ok(R::NotEnded(d));
                    }
                    Ok(R::Unheld)
                }
                Ok(false) => Err(std::io::Error::other("a row socket answers with no authority holding the fence")),
                Err(NotProven::RowSocketUnjudged(detail)) => Err(std::io::Error::other(detail)),
                Err(NotProven::FenceUnavailable) => Err(std::io::Error::other(e.to_string())),
            };
        }
    };

    // An authority reporting `Starting` (a held row is one: `Starting` is also
    // what the storage wait reports) is waited out for activation's bound, then
    // stopped and judged by the absence proof, as a terminal one is.
    let status = match status.phase {
        SupervisorPhase::Starting => {
            let requery = || sot_log::attach_client::supervisor_client::query_status(state_dir).map(|(status, _)| status);
            match settle_starting(requery, || std::thread::sleep(super::activation::ACTIVATION_REPROBE_INTERVAL)) {
                Settle::Settled(status) => status,
                Settle::StillStarting => {
                    return end_still_starting(
                        || {
                            stop_and_end_scope(
                                state_dir,
                                "the authority was still starting after the settle bound",
                                scope.as_deref(),
                                &root,
                                &own,
                            )
                        },
                        || absence_proof(state_dir),
                    );
                }
                Settle::Lost(e) => return Ok(R::NotEnded(format!("the lane went away while starting: {e}"))),
            }
        }
        _ => status,
    };

    match status.phase {
        // Settled above: a phase still `Starting` here would be a bug, and ends nothing.
        SupervisorPhase::Starting => return Ok(R::NotEnded("the authority is still starting".to_string())),
        SupervisorPhase::EndedNoRespawn => {
            // A PRIOR end already landed here and was never stopped
            // (exactly the leak this whole function closes) — a
            // fresh `EndRun` command would only be refused
            // (`Failed{"no leg is currently running"}`, since
            // `EndRun` requires `Lifecycle::Ready`; see
            // `supervisor/authority/mod.rs`'s `handle_command`). Skip the doomed
            // round trip; retry the stop instead of fabricating a
            // verified outcome this call never actually observed.
            if let Err(d) =
                stop_and_end_scope(state_dir, "already ended (EndedNoRespawn) before this call", scope.as_deref(), &root, &own)
            {
                return Ok(R::NotEnded(d));
            }
            return Ok(R::AlreadyEnded);
        }
        SupervisorPhase::Terminal => {
            // No fresh `EndRun` would ever be admitted here
            // (`Lifecycle::Terminal` isn't `Ready`), so the authority
            // is stopped first — but `Terminal` alone does NOT prove
            // the leg died (Codex review, 2026-09-11: it is also
            // reached by the watchdog's own exhausted restart budget,
            // by a failed adoption, or by a kill/wait failure on the
            // leg itself, none of which confirm the leg is gone).
            // This is therefore NOT a confirmed end on its own — the
            // SAME independent [`absence_proof`] the unreachable arm
            // above uses decides whether the row is actually Removable.
            if let Err(d) = stop_and_end_scope(
                state_dir,
                "the authority was terminal before this call reached it",
                scope.as_deref(),
                &root,
                &own,
            ) {
                return Ok(R::NotEnded(d));
            }
            return match absence_proof(state_dir) {
                Ok(true) => Ok(R::Terminal),
                Ok(false) => {
                    Err(std::io::Error::other("a row socket answers with no authority holding the fence (terminal)"))
                }
                Err(NotProven::RowSocketUnjudged(detail)) => Err(std::io::Error::other(detail)),
                Err(NotProven::FenceUnavailable) => Err(std::io::Error::other(
                    "the authority did not release its fence after being stopped",
                )),
            };
        }
        SupervisorPhase::Ready | SupervisorPhase::Ending => {}
    }

    // Ready and Ending name the live run, which fences the end: another run's id is refused.
    let Some(run) = status.run else {
        return Ok(R::NotEnded("the supervisor reports no live run to end".to_string()));
    };
    let outcome = sot_log::attach_client::supervisor_client::end_run(state_dir, &run, reason)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(match outcome {
        O::Ended => match stop_and_end_scope(state_dir, "end_run confirmed", scope.as_deref(), &root, &own) {
            Ok(()) => R::Ended,
            Err(d) => R::NotEnded(d),
        },
        O::Failed(detail) => R::NotEnded(format!("end_run failed: {detail}")),
        O::Refused(detail) => R::NotEnded(format!("end_run refused: {detail}")),
        O::OutcomeUnknown => R::NotEnded("end_run outcome unknown (no answer that the run had ended; read status)".to_string()),
    })
}

/// What waiting out an authority that reports `Starting` found.
enum Settle {
    /// The phase left `Starting`.
    Settled(sot_log::attach_client::supervisor_client::StatusReport),
    /// Still `Starting` after activation's bound of re-probes.
    StillStarting,
    /// The lane stopped answering while waiting.
    Lost(sot_log::Error),
}

/// Re-queries until the phase is not `Starting`, pausing between tries, at
/// most `ACTIVATION_MAX_REPROBES` pauses (the bound `ensure_started` waits a
/// transient phase out for): a settling authority gets the time activation
/// gives one; a held one (the storage wait reports `Starting` too) does not.
fn settle_starting(
    mut requery: impl FnMut() -> sot_log::Result<sot_log::attach_client::supervisor_client::StatusReport>,
    mut pause: impl FnMut(),
) -> Settle {
    use sot_log::SupervisorPhase;
    for pauses in 0..=super::activation::ACTIVATION_MAX_REPROBES {
        match requery() {
            Ok(status) if status.phase != SupervisorPhase::Starting => return Settle::Settled(status),
            Ok(_) => {}
            Err(e) => return Settle::Lost(e),
        }
        if pauses < super::activation::ACTIVATION_MAX_REPROBES {
            pause();
        }
    }
    Settle::StillStarting
}

/// An authority still `Starting` after the settle bound is stopped, then
/// judged by the same absence proof a terminal one is: no authority and no
/// leg holds the row, or the row is kept with the reason. `stop` is
/// `Err(detail)` when the stop or the scope end failed.
fn end_still_starting(
    stop: impl FnOnce() -> Result<(), String>,
    prove: impl FnOnce() -> Result<bool, NotProven>,
) -> std::io::Result<super::EndRunOutcome> {
    if let Err(detail) = stop() {
        return Ok(super::EndRunOutcome::NotEnded(detail));
    }
    match prove() {
        Ok(true) => Ok(super::EndRunOutcome::Unheld),
        Ok(false) => Err(std::io::Error::other("a row socket answers with no authority holding the fence (stopped while starting)")),
        Err(NotProven::RowSocketUnjudged(detail)) => Err(std::io::Error::other(detail)),
        Err(NotProven::FenceUnavailable) => Err(std::io::Error::other(
            "the authority did not release its fence after being stopped while starting",
        )),
    }
}

/// Why [`absence_proof`] could not prove either outcome — distinct
/// variants only so each of [`end_run`]'s TWO call sites (the
/// unreachable-lane arm and the `Terminal` arm) can keep its own
/// honest wording: a fence-stage failure means something else may
/// still hold the AUTHORITY (each caller already knows its own
/// reason to say there), while a socket-check failure carries
/// [`row_socket_silent`]'s own message forward instead of being discarded
/// for a caller-supplied one (Codex review, 2026-09-11 — the old code
/// replaced the check's own error with the outer `query_status`
/// error, losing the actual reason absence wasn't proven).
pub(crate) enum NotProven {
    FenceUnavailable,
    RowSocketUnjudged(String),
}

/// ADR 0043 decision 33's own absence proof, shared by every
/// [`end_run`] arm that reaches a state with no live run to end and
/// so gets no live `end_run` round trip over the lane: the
/// unreachable-lane arm (no supervisor answers at all) and the
/// `Terminal` arm (a lane that DID answer, but whose authority admits
/// no fresh `EndRun` and was just told to stop — Codex review,
/// 2026-09-11: `Terminal` is also reached by watchdog restart-budget
/// exhaustion and by a failed kill/wait on the agent itself, neither of
/// which proves the agent died, so `Terminal` alone is not a confirmed
/// end). A bounded, non-blocking attempt to take `supervisor.lock`
/// settles the AUTHORITY half — acquirable means no supervisor holds
/// this row; released immediately (this call only OBSERVES, it must
/// never itself become the holder) — and [`row_socket_silent`] settles
/// the other half. `Ok(true)`: the fence is free and nothing answers —
/// nothing is running here. `Ok(false)`: a socket answers with no
/// authority holding the fence — reported, never silently orphaned. `Err`:
/// absence is NOT proven either way, so the caller must keep the row
/// rather than guess.
fn absence_proof(state_dir: &Path) -> Result<bool, NotProven> {
    absence_proof_then(state_dir, row_socket_silent, || ()).map(|acted| acted.is_some())
}

/// [`absence_proof`] with its socket check named by the caller and an act run while the supervisor fence is still held:
/// `Ok(Some(_))` is the act's result when the fence was free and `silent` found nothing answering; `Ok(None)` is a socket
/// that answers (the act did not run); `Err` is absence not proven. The act runs under the fence so nothing can take it
/// between the proof and the act (a removal's rename).
pub(crate) fn absence_proof_then<T>(
    state_dir: &Path,
    silent: impl FnOnce(&Path) -> Result<bool, String>,
    act: impl FnOnce() -> T,
) -> Result<Option<T>, NotProven> {
    let _fence =
        sot_log::supervisor::journal::fence::lock_supervisor(state_dir).map_err(|_| NotProven::FenceUnavailable)?;
    if silent(state_dir).map_err(NotProven::RowSocketUnjudged)? {
        Ok(Some(act()))
    } else {
        Ok(None)
    }
    // `_fence` drops here -- observe (and act) only, never stay the holder.
}

/// `end_run`'s missing-state-dir proof: whether `query_status`'s own
/// connect failure `e` is conclusive that NOTHING answers this row's
/// lane, reusing decision 27's own classification
/// (`TransportError::is_endpoint_absent`: `ECONNREFUSED`/`ENOENT`/
/// their Windows equivalents, returned on the FIRST attempt — never
/// the outcome of a retried-but-still-busy endpoint, which means a
/// listener exists). Only the connect step itself produces a
/// `sot_log::Error::Transport` — a challenge that answered `Foreign`/
/// `Undetermined`/`VersionSkew`, or a reply-stage failure, is a
/// DIFFERENT `Error` variant and correctly falls through to `false`
/// here, because each of those means something DID answer. A `true`
/// result, together with the caller's own `!state_dir.is_dir()`
/// check, is the missing directory's own version of
/// [`absence_proof`]: nowhere left for `supervisor.lock` to exist
/// (it lives under `state_dir`), and no supervisor reachable at the one address that does not depend on
/// the directory existing.
fn is_definitely_orphaned(e: &sot_log::Error) -> bool {
    matches!(e, sot_log::Error::Transport(te) if te.is_endpoint_absent())
}

/// The row-socket half of decision 33's destroy proof: [`absence_proof`] calls this only once the supervisor fence is
/// already proven free, never on its own. A supervisor holds the fence for its whole life and its agent ends with it (the
/// kernel's parent-death signal, the hangup, the job), so a free fence leaves nothing to answer on the row's socket; this
/// asks anyway, once, and reads no voyage. `Ok(true)`: nothing answers (the connect was refused or found nothing there,
/// [`is_definitely_orphaned`]). `Ok(false)`: something answers a row whose fence nobody holds -- reported, never silently
/// orphaned. `Err`: the socket could not be judged (a timeout, a challenge that proves nothing), so absence is NOT proven
/// and the caller keeps the row rather than guesses.
pub fn row_socket_silent(state_dir: &Path) -> Result<bool, String> {
    match sot_log::attach_client::supervisor_client::query_status(state_dir) {
        Ok(_) => Ok(false),
        Err(e) if is_definitely_orphaned(&e) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

/// Best-effort `stop` after [`end_run`] confirms there is no more
/// leg to run — see that function's own doc for why this exists and
/// why a failure here is only ever logged, never propagated.
fn stop_and_warn(state_dir: &Path, why: &'static str) {
    if let Err(e) = sot_log::attach_client::supervisor_client::stop(state_dir) {
        tracing::warn!(
            state_dir = ?state_dir, error = %e, why,
            "capsule workspace: stop after end_run failed (resident supervisor leaked)"
        );
    }
}

/// [`stop_and_warn`], then (Linux, A4b) end the row's remembered
/// scopes and the one [`end_run`] captured, so a descendant that left
/// the agent's process group ends with the row. `Err` keeps the row
/// not ended.
fn stop_and_end_scope(state_dir: &Path, why: &'static str, scope: Option<&str>, root: &Path, own: &str) -> Result<(), String> {
    stop_then_end_scope(state_dir, scope, root, own, || stop_and_warn(state_dir, why))
}

/// The graceful `stop`, then the scope end: a live supervisor is never
/// hard-killed ahead of its graceful end, and the kill only finds what
/// the protocol could not reach. `stop` is a parameter so a unit test
/// can see the scope file at the stop.
pub(crate) fn stop_then_end_scope(state_dir: &Path, scope: Option<&str>, root: &Path, own: &str, stop: impl FnOnce()) -> Result<(), String> {
    stop();
    #[cfg(target_os = "linux")]
    {
        use super::row_scope::{end, SCOPE_EMPTY_BOUND};
        end(root, own, state_dir, scope, SCOPE_EMPTY_BOUND)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state_dir, scope, root, own);
        Ok(())
    }
}

#[cfg(test)]
#[path = "end_run_is_definitely_orphaned_tests.rs"]
mod is_definitely_orphaned_tests;

#[cfg(test)]
#[path = "end_run_tests.rs"]
mod tests;
