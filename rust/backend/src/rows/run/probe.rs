//! The row-phase vocabulary the daemon reports, the pure mappings from a supervisor lane's own phase, and `probe`, the one status round trip.

use std::path::{Path, PathBuf};

/// The wire phase string `workspace.list` reports (`WorkspaceListEntry.phase`)
/// for a capsule workspace whose supervisor lane could not be reached at
/// all — connect refused, an undetermined challenge, or a timeout (ADR
/// 0042 L1a: "failure -> unreachable"). Distinct from every
/// [`sot_log::lane::wire::SupervisorPhase`] variant, which are all states of a
/// lane that DID answer, and from [`FOREIGN_PHASE`] (ADR 0030 §8 decision
/// 31c) below — a challenge that specifically proves foreign now gets its
/// own phase rather than folding in here.
pub const UNREACHABLE_PHASE: &str = "unreachable";

/// The wire phase string for a capsule workspace whose supervisor lane
/// DID answer — but with `version_skew`: it is held by a supervisor
/// speaking ANOTHER lane protocol (ADR 0030 §8 decision 31c, ADR 0043
/// decision 31; ADR 0045 decision 7 — the gate is `proto` alone now, not
/// build; decision 9: adopting a supervisor of another BUILD is
/// ordinary, only a proto mismatch is foreign). This daemon can never
/// attach, adopt, end, or destroy such a row: end the row from a client
/// of the proto it speaks, or kill only its `sot-capsule supervise`
/// process and attach again — the leg and the agent in it survive and
/// the next attach's `ensure_started` resumes and adopts. Deliberately
/// its own phase rather than folding into [`UNREACHABLE_PHASE`]: the
/// lane DID answer, which is exactly the fact
/// `note_if_foreign` already detected and used to be discarded one line
/// before the wire (2026-09-08 field incident) — this is that fact,
/// finally on the wire. `phase_of` sets it on the SAME branch
/// `note_if_foreign` already recognizes by text-matching "foreign" in
/// `query_status`'s error — no new detection, only a new destination for
/// a fact this daemon already had.
pub const FOREIGN_PHASE: &str = "foreign";

/// The wire phase string for a capsule workspace with no published
/// voyage pointer (`<state_dir>/drawer.voyage`, `sot_log::supervisor::journal::pointer` —
/// ADR 0041 Lifecycle's write-once durable fact that a voyage exists).
/// Rule B (shrink round): the POINTER, not directory presence, is the
/// discriminator — a state directory can exist with no pointer ever
/// published to it (the exact pre-pointer crash window ADR 0041 names,
/// or simply a row `resume_all` correctly never touched because it had
/// none), and that reads identically to a workspace whose directory was
/// never created at all: neither has ever had a real run. Distinct from
/// [`UNREACHABLE_PHASE`]: a workspace WITH a published pointer means a
/// supervisor did reach a real run at least once, so a lane that fails to
/// answer against it stays "unreachable" — `query_status`'s own doc
/// deliberately folds every such failure (connect refused, a foreign/
/// undetermined challenge, a timeout) into one `Err` without saying
/// which, so a query against a workspace WITH a pointer can never be
/// reclassified as "never started" either. One narrow race this accepts
/// (Codex round, PR #172): a `workspace.list` landing in the brief window
/// where a resumed supervisor's pointer is still being (re-)published
/// reads "stopped" too — bounded by the spawn call itself and
/// self-correcting on the very next list once the pointer (and the
/// supervisor behind it) exists.
pub const NEVER_STARTED_PHASE: &str = "stopped";

/// Whether a capsule workspace's supervisor lane is even worth querying,
/// given whether its voyage pointer exists — pure, no I/O itself (the
/// caller supplies `pointer_exists`, e.g. `phase_of`'s own
/// `sot_log::supervisor::journal::pointer::pointer_path(state_dir).is_file()`). `None` means
/// "query it, we can't tell from this alone"; `Some(..)` short-circuits a
/// connect attempt that cannot possibly succeed — no pipe was ever bound
/// for a workspace whose pointer was never published, so `phase_of` skips
/// straight to [`NEVER_STARTED_PHASE`] rather than waiting out a connect
/// budget destined to fail. The pointer lives INSIDE the state dir, so
/// its absence subsumes "no state dir at all" (the check this replaces)
/// as well as "a state dir exists but nothing was ever durably published
/// to it" — both read as never started.
pub fn phase_for_missing_pointer(pointer_exists: bool) -> Option<&'static str> {
    (!pointer_exists).then_some(NEVER_STARTED_PHASE)
}

/// Map the supervisor lane's own phase to the wire string
/// `workspace.list` reports — snake_case, matching every other
/// wire-enum-as-string in this protocol (`repl_state`, `agent_state`).
/// Portable: [`sot_log::lane::wire`] has no OS dependency (see that crate's own
/// module doc), so this needs no `#[cfg(windows)]` either, and the pure
/// unit tests below exercise it directly on Linux.
pub fn phase_str(phase: sot_log::lane::wire::SupervisorPhase) -> &'static str {
    use sot_log::lane::wire::SupervisorPhase;
    match phase {
        SupervisorPhase::Starting => "starting",
        SupervisorPhase::Ready => "ready",
        SupervisorPhase::Ending => "ending",
        SupervisorPhase::EndedNoRespawn => "ended_no_respawn",
        SupervisorPhase::Terminal => "terminal",
    }
}

/// Converts to the local `Phase` (R10); [`phase_str`] stays for the wire mapping.
pub(crate) fn local_phase(phase: sot_log::lane::wire::SupervisorPhase) -> crate::rows::workspace::Phase {
    use crate::rows::workspace::Phase;
    use sot_log::lane::wire::SupervisorPhase as SP;
    match phase {
        SP::Starting => Phase::Starting,
        SP::Ready => Phase::Ready,
        SP::Ending => Phase::Ending,
        SP::EndedNoRespawn => Phase::EndedNoRespawn,
        SP::Terminal => Phase::Terminal,
    }
}

/// One status round trip: wire string plus the identity-carrying `Observation` it implies. BLOCKING.
pub fn probe(state_dir: &Path) -> (&'static str, crate::rows::workspace::Observation) {
    use crate::rows::workspace::{Observation, SupervisorIdentity};
    if let Some(phase) =
        super::phase_for_missing_pointer(sot_log::supervisor::journal::pointer::pointer_path(state_dir).is_file())
    {
        return (phase, Observation::Stopped);
    }
    match sot_log::attach_client::supervisor_client::query_status(state_dir) {
        // The retained process handle (the second element) is not
        // this caller's concern -- a one-shot phase probe, dropped
        // (closing the handle) the instant this returns.
        Ok((report, _process)) => {
            let observation = Observation::Phase {
                phase: super::local_phase(report.phase),
                supervisor: SupervisorIdentity { pid: report.pid, created: report.created },
                voyage: report.voyage.as_deref().and_then(|v| v.parse().ok()),
            };
            (super::phase_str(report.phase), observation)
        }
        // Typed, not text (ADR 0030 §8 decision 31c): `VersionSkew`
        // is the ONLY `sot_log::Error` variant `query_status` returns
        // for a lane that answered but refused this build. Every
        // other error -- a malformed reply, a timeout, connect
        // refused -- stays `UNREACHABLE_PHASE`.
        Err(sot_log::Error::VersionSkew) => {
            note_version_skew(state_dir);
            (super::FOREIGN_PHASE, Observation::Foreign)
        }
        Err(e) => {
            tracing::debug!(state_dir = ?state_dir, error = %e, "capsule workspace: supervisor lane unreachable");
            (super::UNREACHABLE_PHASE, Observation::Failed)
        }
    }
}

/// [`probe`]'s wire string alone, for a caller with no row to observe into (`watchdog_may_act`).
pub fn phase_of(state_dir: &Path) -> &'static str {
    probe(state_dir).0
}

/// Log ONCE per row per daemon lifetime that a capsule row's
/// supervisor refused this daemon's hello (ADR 0030 §8 decision 31c;
/// the gate itself is superseded by ADR 0045 decision 7) — called
/// only once the caller has ALREADY typed-matched
/// `sot_log::Error::VersionSkew`, so this never fires on a merely
/// unreachable lane. Wording covers BOTH migration-window causes
/// (Codex review, 2026-09-11): a genuine lane-protocol mismatch, or
/// an OLD (pre-ADR-0045) supervisor still refusing on build — this
/// daemon cannot tell which from the wire alone, so it never claims
/// to. `phase_of` is also the list poll's own probe, so this dedupes
/// on `state_dir` rather than logging every poll.
fn note_version_skew(state_dir: &Path) {
    use std::sync::{Mutex, OnceLock};
    static NOTED: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    let mut noted = NOTED.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    if noted.insert(state_dir.to_path_buf()) {
        tracing::warn!(
            state_dir = ?state_dir,
            "the row's supervisor refused this client (another lane protocol, or a supervisor from before \
             the protocol-only gate); end the row and recreate it, or kill only its `sot-capsule supervise` \
             process and attach the row again (the run leg and its agent survive and are adopted)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_str_is_total_and_snake_case() {
        use sot_log::lane::wire::SupervisorPhase;
        assert_eq!(phase_str(SupervisorPhase::Starting), "starting");
        assert_eq!(phase_str(SupervisorPhase::Ready), "ready");
        assert_eq!(phase_str(SupervisorPhase::Ending), "ending");
        assert_eq!(phase_str(SupervisorPhase::EndedNoRespawn), "ended_no_respawn");
        assert_eq!(phase_str(SupervisorPhase::Terminal), "terminal");
    }

    #[test]
    fn wire_constants_and_phase_str_are_the_phases_strings() {
        use crate::rows::workspace::Phase;
        use sot_log::lane::wire::SupervisorPhase;
        assert_eq!(UNREACHABLE_PHASE, "unreachable");
        assert_eq!(FOREIGN_PHASE, "foreign");
        assert_eq!(NEVER_STARTED_PHASE, "stopped");
        assert_eq!(UNREACHABLE_PHASE, Phase::Unreachable.as_wire_str());
        assert_eq!(FOREIGN_PHASE, Phase::Foreign.as_wire_str());
        assert_eq!(NEVER_STARTED_PHASE, Phase::Stopped.as_wire_str());
        for p in [
            SupervisorPhase::Starting,
            SupervisorPhase::Ready,
            SupervisorPhase::Ending,
            SupervisorPhase::EndedNoRespawn,
            SupervisorPhase::Terminal,
        ] {
            assert_eq!(phase_str(p), local_phase(p).as_wire_str());
        }
    }

    #[test]
    fn never_started_phase_is_distinct_from_unreachable_and_every_answered_phase() {
        // First live shakedown fix: a capsule workspace nobody has ever
        // started must read as quietly "stopped", not as the loud
        // "unreachable" a query FAILURE reports — the two must never
        // collide with each other or with any answered lifecycle phase.
        use sot_log::lane::wire::SupervisorPhase;
        assert_ne!(NEVER_STARTED_PHASE, UNREACHABLE_PHASE);
        for p in [
            SupervisorPhase::Starting,
            SupervisorPhase::Ready,
            SupervisorPhase::Ending,
            SupervisorPhase::EndedNoRespawn,
            SupervisorPhase::Terminal,
        ] {
            assert_ne!(phase_str(p), NEVER_STARTED_PHASE);
        }
    }

    #[test]
    fn phase_for_missing_pointer_only_fires_when_the_pointer_is_absent() {
        // Rule B: a published pointer means a supervisor reached a real
        // run at least once — that case defers to the real query (`None`)
        // rather than guessing; only a genuinely absent pointer
        // short-circuits to `NEVER_STARTED_PHASE`.
        assert_eq!(
            phase_for_missing_pointer(false),
            Some(NEVER_STARTED_PHASE)
        );
        assert_eq!(phase_for_missing_pointer(true), None);
    }

    #[test]
    fn unreachable_phase_is_distinct_from_every_answered_phase() {
        use sot_log::lane::wire::SupervisorPhase;
        for p in [
            SupervisorPhase::Starting,
            SupervisorPhase::Ready,
            SupervisorPhase::Ending,
            SupervisorPhase::EndedNoRespawn,
            SupervisorPhase::Terminal,
        ] {
            assert_ne!(phase_str(p), UNREACHABLE_PHASE);
        }
    }
}
