//! `end_run`: ending a capsule row's run, with the destroy proof (no authority and no leg left) before a row is reported ended.

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
    /// The run ended and its record verified green.
    RecordVerified,
    /// The marker committed but the O(retained history) verify walk
    /// hadn't finished within the ADR's 90s cutoff — still safe to treat
    /// as "ended" (the marker itself is the irrevocable acceptance).
    RecordClosed,
    /// The lane was ALREADY resting in `EndedNoRespawn` — recovered via
    /// the leg's own end-marker alone, NEVER via `verify_voyage`, so
    /// this must not be reported as `RecordVerified`/`RecordClosed`.
    AlreadyEnded,
    /// The authority had already reached `Terminal` before this call
    /// ever reached it — its own internal flap/retry budget exhausted
    /// (`FLAP_THRESHOLD`, `rust/log/src/supervisor/`), most often an
    /// agent argv that can never launch (e.g. `claude` missing from
    /// PATH). A `Terminal` authority admits no fresh `EndRun` anyway
    /// (`supervisor/authority/mod.rs`'s `handle_command` gates `EndRun` on
    /// `Lifecycle::Ready`) — so this sends `stop` instead (admitted
    /// unconditionally, regardless of lifecycle: `SupervisorOp::Stop`'s
    /// own admission has no lifecycle gate) and waits for its confirmed
    /// exit. Without this arm the row was UNENDABLE: `workspace.destroy`
    /// kept reporting `NotEnded` forever, because nothing ever told the
    /// stuck authority to stop.
    ///
    /// `Terminal` alone does NOT prove the leg died (Codex review,
    /// 2026-09-11: watchdog restart-budget exhaustion, a failed
    /// adoption, or a kill/wait failure on the leg itself can all reach
    /// `Terminal` with a leg still running) — this variant is reported
    /// ONLY once the SAME [`absence_proof`] `Unheld` uses has
    /// independently confirmed no leg holds the voyage either; a leg
    /// still present is reported (`end_run` keeps the row), never
    /// silently orphaned.
    Terminal,
    /// The lane answered `phase: Starting` (voyage may still be `None`
    /// — only set once Recovering completes) — NEVER "not running"; a
    /// run may be about to (or already did) start. Retry.
    Starting,
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
    /// [`leg_absent`] independently proved no LEG holds the
    /// voyage's own `writer.lock` either (`voyage.rs`). No authority AND
    /// no leg is safe to treat as `Removable`, same as `Terminal`.
    /// Fence free but a leg still present is NOT this variant — a leg
    /// with no authority left to end it is reported instead of silently
    /// orphaned (see `end_run`'s own doc). Distinct from `NotEnded`: that
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
    /// variant proves absence by taking the fence and the writer lock;
    /// neither lock can even be attempted here because both live INSIDE
    /// the missing directory (`fence.rs`, `voyage_root_path`) — so there
    /// is nowhere left for either to exist, let alone be held. This row
    /// has no durable record and no supervisor that could be alive under
    /// this daemon's state root — safe to remove, same as `Unheld`, but
    /// reported under its own name (`orphan_removed`) rather than folded
    /// into "no supervisor held the row", which would misstate that a
    /// row here ever really ran under this daemon.
    Orphaned,
}

/// `workspace.delete` on a capsule workspace (and the default row's
/// end-run path): send `end_run {reason, voyage}` on the lane,
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
pub fn end_run(
    state_dir: &Path,
    reason: &str,
    root_canonicalized: bool,
) -> std::io::Result<super::EndRunOutcome> {
    use super::EndRunOutcome as R;
    use sot_log::attach_client::supervisor_client::EndRunOutcome as O;
    use sot_log::lane::wire::SupervisorPhase;

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
            // of BOTH authority (fence) and leg (writer.lock) — a
            // missing state dir proves neither BY ITSELF, and fence
            // creation there fails outright (`CREATE_NEW` needs the
            // directory), so `absence_proof` below cannot even be
            // attempted. It is checked and reported FIRST, distinct
            // from every other "lane unreachable" case — never a
            // licence to recreate anything (`leg_absent`'s own
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
                    // voyage's `writer.lock` live INSIDE `state_dir`
                    // (`fence.rs`, `voyage_root_path`), so if the
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
                Ok(false) => Err(std::io::Error::other("a leg is running with no authority")),
                Err(NotProven::LegCheckFailed(detail)) => Err(std::io::Error::other(detail)),
                Err(NotProven::FenceUnavailable) => Err(std::io::Error::other(e.to_string())),
            };
        }
    };

    match status.phase {
        SupervisorPhase::Starting => return Ok(R::Starting),
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
                    Err(std::io::Error::other("a leg is running with no authority (terminal)"))
                }
                Err(NotProven::LegCheckFailed(detail)) => Err(std::io::Error::other(detail)),
                Err(NotProven::FenceUnavailable) => Err(std::io::Error::other(
                    "the authority did not release its fence after being stopped",
                )),
            };
        }
        SupervisorPhase::Ready | SupervisorPhase::Ending => {}
    }

    // Ready/Ending are only reachable once Recovering's own Done arm
    // has set `authority.voyage_id` (`supervisor/authority/`), so this is
    // always populated here.
    let voyage = status
        .voyage
        .expect("Ready/Ending implies a voyage_id (supervisor.rs's own recovery transition)");
    let outcome = sot_log::attach_client::supervisor_client::end_run(state_dir, &voyage, reason)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(match outcome {
        O::RecordVerified => match stop_and_end_scope(state_dir, "end_run confirmed verified", scope.as_deref(), &root, &own) {
            Ok(()) => R::RecordVerified,
            Err(d) => R::NotEnded(d),
        },
        O::RecordClosed => match stop_and_end_scope(state_dir, "end_run confirmed closed", scope.as_deref(), &root, &own) {
            Ok(()) => R::RecordClosed,
            Err(d) => R::NotEnded(d),
        },
        O::Failed(detail) => R::NotEnded(format!("end_run failed: {detail}")),
        O::Refused(detail) => R::NotEnded(format!("end_run refused: {detail}")),
        O::OutcomeUnknown => R::NotEnded(
            "end_run outcome unknown (the ADR's own 90s cutoff elapsed with no terminal reply)"
                .to_string(),
        ),
    })
}

/// Why [`absence_proof`] could not prove either outcome — distinct
/// variants only so each of [`end_run`]'s TWO call sites (the
/// unreachable-lane arm and the `Terminal` arm) can keep its own
/// honest wording: a fence-stage failure means something else may
/// still hold the AUTHORITY (each caller already knows its own
/// reason to say there), while a leg-check failure carries
/// [`leg_absent`]'s own message forward instead of being discarded
/// for a caller-supplied one (Codex review, 2026-09-11 — the old code
/// replaced `leg_absent`'s own error with the outer `query_status`
/// error, losing the actual reason absence wasn't proven).
enum NotProven {
    FenceUnavailable,
    LegCheckFailed(String),
}

/// ADR 0043 decision 33's own absence proof, shared by every
/// [`end_run`] arm that reaches a state with no live leg to end and
/// so gets no live `end_run` round trip over the lane: the
/// unreachable-lane arm (no supervisor answers at all) and the
/// `Terminal` arm (a lane that DID answer, but whose authority admits
/// no fresh `EndRun` and was just told to stop — Codex review,
/// 2026-09-11: `Terminal` is also reached by watchdog restart-budget
/// exhaustion and by a failed kill/wait on the leg itself, neither of
/// which proves the leg died, so `Terminal` alone is not a confirmed
/// end). A bounded, non-blocking attempt to take `supervisor.lock`
/// settles the AUTHORITY half — acquirable means no supervisor holds
/// this row; released immediately (this call only OBSERVES, it must
/// never itself become the holder) — and [`leg_absent`] independently
/// settles the LEG half. `Ok(true)`: neither is held — nothing is
/// running here. `Ok(false)`: the leg is running with no authority
/// left to end it — reported, never silently orphaned. `Err`:
/// absence is NOT proven either way, so the caller must keep the row
/// rather than guess.
fn absence_proof(state_dir: &Path) -> Result<bool, NotProven> {
    let _fence =
        sot_log::supervisor::journal::fence::lock_supervisor(state_dir).map_err(|_| NotProven::FenceUnavailable)?;
    leg_absent(state_dir).map_err(NotProven::LegCheckFailed)
    // `_fence` drops here, right after `leg_absent`'s own single
    // observation -- observe only, never become the holder.
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
/// [`absence_proof`]: nowhere left for `supervisor.lock` or any
/// voyage's `writer.lock` to exist (both live under `state_dir`), and
/// no supervisor reachable at the one address that does not depend on
/// the directory existing.
fn is_definitely_orphaned(e: &sot_log::Error) -> bool {
    matches!(e, sot_log::Error::Transport(te) if te.is_endpoint_absent())
}

/// Whether a `lock_writer` failure is genuine contention (its OWN
/// bounded-retry exhaustion, `host/`) rather than some OTHER
/// refusal that happens to share `Error::State`'s shape — Windows:
/// `open_lock_file`'s reparse-point refusal is the one other producer
/// of `Error::State` on this exact call (Codex review, 2026-09-11:
/// conflating the two used to report a live leg for what was actually
/// a security refusal). Matched on `lock_writer`'s own fixed message
/// prefix rather than a new `sot_log::Error` variant — that ONE
/// crate-wide `State` variant already serves dozens of unrelated call
/// sites (see its own doc), so a new variant there is a much wider
/// change than this one call site needs.
pub fn is_lock_contention(detail: &str) -> bool {
    detail.starts_with("lock held by another process:")
}

/// The LEG half of decision 33's destroy proof — [`absence_proof`]
/// calls this only once the AUTHORITY half (the supervisor fence) is
/// already proven free, never on its own. Reads the published
/// pointer to find which voyage the row last bound, then takes the
/// SAME bounded, non-blocking primitive `open_for_writing` itself
/// uses on that voyage's `writer.lock` (`voyage.rs`) — held by a live
/// leg, never by the authority — and releases it at once (this call
/// only OBSERVES, it must never become the holder). `Ok(true)`: the
/// lock was acquirable — no leg holds this voyage. `Ok(false)`: the
/// lock is held — a leg lives with no authority left to end it.
/// `Err`: the pointer itself is not a valid voyage id, or the lock
/// attempt failed for a reason OTHER than contention — either way,
/// absence is NOT proven, so the caller must keep the row rather than
/// guess.
pub fn leg_absent(state_dir: &Path) -> Result<bool, String> {
    let voyage = match sot_log::supervisor::journal::pointer::validate(state_dir) {
        sot_log::supervisor::journal::pointer::PointerState::Valid(id) => id,
        other => return Err(format!("voyage pointer is not valid: {other:?}")),
    };
    let root = sot_log::supervisor::voyage_root_path(state_dir, &voyage);
    match sot_log::lock_writer(&root.join(sot_log::store::voyage::WRITER_LOCK)) {
        Ok(lock) => {
            drop(lock); // observe only -- never become the holder
            Ok(true)
        }
        Err(sot_log::Error::State(detail)) if is_lock_contention(&detail) => Ok(false),
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
mod is_definitely_orphaned_tests {
    use super::*;
    use sot_log::lane::transport::TransportError;

    fn io_transport(kind: std::io::ErrorKind) -> sot_log::Error {
        sot_log::Error::Transport(TransportError::Io {
            op: "connect",
            source: std::io::Error::new(kind, "test"),
        })
    }

    #[test]
    fn no_listener_at_all_is_orphaned() {
        // Decision 27's own "absent" shapes -- what `query_status`'s
        // connect step actually returns on the first attempt when
        // nothing is bound at this row's lane address at all.
        assert!(is_definitely_orphaned(&io_transport(std::io::ErrorKind::NotFound)));
        assert!(is_definitely_orphaned(&io_transport(std::io::ErrorKind::ConnectionRefused)));
    }

    #[test]
    fn a_reachable_but_refusing_lane_is_never_orphaned() {
        // The lane DID answer (a foreign build, a version skew, or
        // some other live-but-unresponsive shape) -- none of these
        // may ever be folded into "nothing is running here".
        assert!(!is_definitely_orphaned(&sot_log::Error::VersionSkew));
        assert!(!is_definitely_orphaned(&sot_log::Error::State(
            "supervisor lane challenge: foreign".to_string()
        )));
    }

    #[test]
    fn a_busy_or_timed_out_endpoint_is_never_orphaned() {
        // Decision 27: a busy endpoint is retried WITHIN the connect
        // bound and only surfaces as `Err` once genuinely ambiguous
        // (e.g. a timeout) -- that must stay unproven, never treated
        // as absent.
        assert!(!is_definitely_orphaned(&io_transport(std::io::ErrorKind::TimedOut)));
        assert!(!is_definitely_orphaned(&io_transport(std::io::ErrorKind::PermissionDenied)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Field defect (v0.6.0-rc.12): a supervisor that died out from under
    // a row (e.g. a daemon-pair converge that ended the old build's
    // supervisor) left the state dir holding only a lock FILE, and
    // `end_run` used to treat every unreachable lane identically -- kept
    // forever, with no way to tell a merely-unresponsive live holder
    // from no holder at all. `query_status` fails against this temp dir
    // in every case below (nothing is listening on its lane) -- decision
    // 33 needs BOTH the fence AND the leg independently proven absent
    // before `Unheld`; a missing pointer (nothing to check the leg
    // against), a present leg, or a still-held fence each keep the row
    // instead.
    #[test]
    fn end_run_is_unheld_only_when_both_the_fence_and_the_leg_are_proven_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path();

        // No supervisor.lock AND no published pointer at all -- the
        // fence is free, but there is nothing to prove the leg absent
        // against: uncertain, never fabricated as `Unheld`.
        match end_run(state_dir, "test reason", true) {
            Err(_) => {}
            Ok(outcome) => panic!("expected Err with no pointer to check the leg against: {outcome:?}"),
        }

        // A published pointer naming a real voyage root whose own
        // `writer.lock` exists and is free -- BOTH halves now proven
        // absent.
        let voyage_id = "a1b2c3d4-e5f6-4890-9abc-def012345678";
        sot_log::supervisor::journal::pointer::publish(state_dir, voyage_id).expect("publish the pointer");
        let voyage_root = sot_log::supervisor::voyage_root_path(state_dir, voyage_id);
        std::fs::create_dir_all(&voyage_root).expect("voyage root");
        std::fs::write(voyage_root.join("writer.lock"), b"").expect("writer.lock file");
        match end_run(state_dir, "test reason", true) {
            Ok(super::EndRunOutcome::Unheld) => {}
            other => panic!("expected Ok(Unheld) with no authority and no leg: {other:?}"),
        }

        // A leg holds the voyage's own `writer.lock` -- reported
        // instead of silently orphaned.
        let leg = sot_log::lock_writer(&voyage_root.join("writer.lock")).expect("take the writer lock");
        match end_run(state_dir, "test reason", true) {
            Err(_) => {}
            Ok(outcome) => panic!("expected Err while a leg holds the row with no authority: {outcome:?}"),
        }
        drop(leg);

        // Something else holds the SUPERVISOR fence right now -- the
        // lock attempt must fail regardless of the leg, so `end_run`
        // keeps the unreachable-lane refusal (Err) rather than
        // fabricating `Unheld` out from under a live holder.
        let holder = sot_log::supervisor::journal::fence::lock_supervisor(state_dir).expect("take the fence");
        match end_run(state_dir, "test reason", true) {
            Err(_) => {}
            Ok(outcome) => {
                panic!("expected the unchanged unreachable-lane Err while the fence is held: {outcome:?}")
            }
        }
        drop(holder);
    }

    // The direct writer.lock cases (`leg_absent`'s own Ok(true)/Ok(false)
    // on a real held/free lock) are already exercised through `end_run`
    // by `end_run_is_unheld_only_when_both_the_fence_and_the_leg_are_proven_absent`
    // above (including the no-pointer-published Err case) -- this test
    // instead targets what `leg_absent` cannot organically produce on
    // Linux at all: the OTHER, non-contention refusal `lock_writer` can
    // report (Windows' reparse-point check, `host/`) sharing the SAME
    // `Error::State` shape as genuine contention. `is_lock_contention` is
    // the pure predicate that tells them apart (Codex review,
    // 2026-09-11); this is its regression test -- pure string matching,
    // independent of any real lock file.
    #[test]
    fn lock_contention_is_recognized_only_by_its_own_message() {
        assert!(
            is_lock_contention("lock held by another process: \"/tmp/x/writer.lock\""),
            "lock_writer's own bounded-retry-exhaustion text must be recognized as contention"
        );
        assert!(
            !is_lock_contention(
                "writer.lock at \"/tmp/x/writer.lock\" is a reparse point — refusing a redirected fence"
            ),
            "a reparse-point refusal is not contention -- absence must stay unproven, not read as \"a leg lives\""
        );
        assert!(!is_lock_contention("some unrelated State error"));
    }
}
