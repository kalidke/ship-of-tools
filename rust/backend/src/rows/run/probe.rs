//! The row-phase vocabulary the daemon reports, the pure mappings from a supervisor lane's own phase, and `probe`, the one status round trip.

use std::path::{Path, PathBuf};

/// The wire phase string `workspace.list` reports (`WorkspaceListEntry.phase`)
/// for a capsule workspace whose supervisor lane could not be reached at
/// all — connect refused, an undetermined challenge, or a timeout (ADR
/// 0042 L1a: "failure -> unreachable"). Distinct from every
/// [`sot_log::SupervisorPhase`] variant, which are all states of a
/// lane that DID answer, and from [`FOREIGN_PHASE`] (ADR 0030 §8 decision
/// 31c) below — a challenge that specifically proves foreign now gets its
/// own phase rather than folding in here.
pub const UNREACHABLE_PHASE: &str = crate::rows::workspace::Phase::Unreachable.as_wire_str();

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
pub const FOREIGN_PHASE: &str = crate::rows::workspace::Phase::Foreign.as_wire_str();

/// The wire phase string for a capsule workspace whose state folder holds no supervisor fence file
/// (`<state_dir>/supervisor.lock`, `sot_log::supervisor::journal::fence`).
/// The fence file, not directory presence, is the discriminator: the daemon claims the fence before it forks a supervisor
/// and a supervisor takes it before anything else, so a folder holding the file had a supervisor started in it, whether
/// or not that supervisor got as far as publishing anything. A folder without one (the directory missing, or present and
/// never started in) reads as never started: neither has ever had a start. Distinct from [`UNREACHABLE_PHASE`]: a folder
/// WITH a fence file means a start was made, so a row socket that fails to answer against it stays "unreachable"
/// (`query_status` folds every such failure -- connect refused, a foreign or undetermined challenge, a timeout -- into one
/// `Err` without saying which), and a start whose fork failed after the claim reads the same. One narrow race this
/// accepts: a `workspace.list` that lands before the daemon's claim for a first start reads "stopped" too, which the next
/// list corrects.
pub const NEVER_STARTED_PHASE: &str = crate::rows::workspace::Phase::Stopped.as_wire_str();

/// Whether a capsule workspace's supervisor is even worth querying, given whether its fence file exists: pure, no I/O
/// itself (the caller supplies `fence_exists`, e.g. `phase_of`'s own `fence_file_exists`). `None` means "query it, we can't
/// tell from this alone"; `Some(..)` short-circuits a connect attempt that cannot possibly succeed -- no row socket was
/// ever bound for a folder no start was made in -- so `phase_of` skips straight to [`NEVER_STARTED_PHASE`] rather than
/// waiting out a connect budget destined to fail.
pub fn phase_for_missing_fence(fence_exists: bool) -> Option<&'static str> {
    (!fence_exists).then_some(NEVER_STARTED_PHASE)
}

/// Whether `state_dir` holds the supervisor fence file: the one fact that a start was made in it.
pub fn fence_file_exists(state_dir: &Path) -> bool {
    sot_log::supervisor::journal::fence::supervisor_lock_path(state_dir).is_file()
}

/// Map the supervisor's own phase to the wire string
/// `workspace.list` reports — snake_case, matching every other
/// wire-enum-as-string in this protocol (`repl_state`, `agent_state`).
/// Portable: [`sot_log::SupervisorPhase`] has no OS dependency, so this needs no `#[cfg(windows)]` either, and the
/// pure unit tests below exercise it directly on Linux.
pub fn phase_str(phase: sot_log::SupervisorPhase) -> &'static str {
    local_phase(phase).as_wire_str()
}

/// Converts to the local `Phase` (R10); [`phase_str`] stays for the wire mapping.
pub(crate) fn local_phase(phase: sot_log::SupervisorPhase) -> crate::rows::workspace::Phase {
    use crate::rows::workspace::Phase;
    use sot_log::SupervisorPhase as SP;
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
    if let Some(phase) = super::phase_for_missing_fence(super::fence_file_exists(state_dir)) {
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
                run: report.run.as_deref().and_then(|r| r.parse().ok()),
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
            "the row's supervisor speaks another row protocol number than this client; end the row and recreate it. Killing its `sot-capsule supervise` process \
             ends its agent with it; attaching the row again then resumes the row in a new run"
        );
    }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
