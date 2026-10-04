// capsule_workspace.rs — ADR 0042 slice L1a / ADR 0043 decision 22: the
// daemon's capsule workspace runtime, on Windows AND Linux. One
// `sot-capsule supervise <state-dir>` authority per capsule workspace,
// spawned DETACHED so it survives the daemon's own exit — the daemon is
// never its kill domain. Which platform is chosen is exactly TWO forks
// inside `mod runtime` (the capsule executable's name and the detach
// mechanism — that module's own doc says why this used to be three)
// — everything else in that module is byte-identical on both platforms.
// `runtime: "tmux"`
// rows stay exactly what they are today; this module never touches
// them. ADR 0042's rule now holds on Linux too (L6 / this repo's B6
// lane, once the bridge gave a capsule row a remote attach path):
// `workspace.create`'s absent `runtime` resolves to "capsule" here
// just as it does on Windows; `"tmux"` still exists for an operator's
// explicit ask, and old rows retire by attrition.
//
// Split deliberately into PURE helpers (no OS call: the state-dir path
// arithmetic, the phase-to-wire-string mapping, the agent argv choice)
// and the platform runtime (spawning, watching, querying, ending a
// supervisor over `sot_log::supervisor_client`). The pure half is
// compiled and unit-tested on every platform — ADR 0042 L1a's own gate
// runs `cargo test --workspace` on Linux, and gating path/string
// arithmetic behind `#[cfg(windows)]` would only prevent that gate from
// ever exercising it. On a host that is neither Windows nor Linux
// nothing in this module is called at all: `workspace.create` keeps
// today's tmux path unchanged (see `workspaces.rs`/`handlers.rs`).

#[cfg(test)]
use std::path::Path;

pub use crate::agents::argv::agent_argv;
#[cfg(unix)]
pub use crate::agents::argv::agent_exec_argv;
pub use crate::agents::argv::claude_resume_argv;
#[cfg(not(windows))]
pub use crate::agents::env::agent_env;
pub use crate::agents::env::{capsule_supervisor_env, NESTING_ENV_VARS_TO_SCRUB};

pub(crate) use crate::rows::spawn::detach::{capsule_sibling_present, StartMode};
#[cfg(target_os = "linux")]
pub(crate) use crate::rows::spawn::row_scope;
#[cfg(target_os = "macos")]
pub(crate) use crate::rows::spawn::state_root::macos_only;
pub(crate) use crate::rows::spawn::state_root::{
    qualified_state_root, state_dir_for, state_root_inside_project, STATE_ROOT_HINT,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationIntent {
    /// May resume, and may retire+reset an ended run.
    Selection,
    /// May resume, but never resets an ended run.
    Reconnect,
}

/// The wire phase string `workspace.list` reports (`WorkspaceListEntry.phase`)
/// for a capsule workspace whose supervisor lane could not be reached at
/// all — connect refused, an undetermined challenge, or a timeout (ADR
/// 0042 L1a: "failure -> unreachable"). Distinct from every
/// [`sot_log::wire::SupervisorPhase`] variant, which are all states of a
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
/// voyage pointer (`<state_dir>/drawer.voyage`, `sot_log::pointer` —
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
#[cfg_attr(not(windows), allow(dead_code))]
pub const NEVER_STARTED_PHASE: &str = "stopped";

/// Whether a capsule workspace's supervisor lane is even worth querying,
/// given whether its voyage pointer exists — pure, no I/O itself (the
/// caller supplies `pointer_exists`, e.g. `phase_of`'s own
/// `sot_log::pointer::pointer_path(state_dir).is_file()`). `None` means
/// "query it, we can't tell from this alone"; `Some(..)` short-circuits a
/// connect attempt that cannot possibly succeed — no pipe was ever bound
/// for a workspace whose pointer was never published, so `phase_of` skips
/// straight to [`NEVER_STARTED_PHASE`] rather than waiting out a connect
/// budget destined to fail. The pointer lives INSIDE the state dir, so
/// its absence subsumes "no state dir at all" (the check this replaces)
/// as well as "a state dir exists but nothing was ever durably published
/// to it" — both read as never started.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn phase_for_missing_pointer(pointer_exists: bool) -> Option<&'static str> {
    (!pointer_exists).then_some(NEVER_STARTED_PHASE)
}

/// Map the supervisor lane's own phase to the wire string
/// `workspace.list` reports — snake_case, matching every other
/// wire-enum-as-string in this protocol (`repl_state`, `agent_state`).
/// Portable: [`sot_log::wire`] has no OS dependency (see that crate's own
/// module doc), so this needs no `#[cfg(windows)]` either, and the pure
/// unit tests below exercise it directly on Linux.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn phase_str(phase: sot_log::wire::SupervisorPhase) -> &'static str {
    use sot_log::wire::SupervisorPhase;
    match phase {
        SupervisorPhase::Starting => "starting",
        SupervisorPhase::Ready => "ready",
        SupervisorPhase::Ending => "ending",
        SupervisorPhase::EndedNoRespawn => "ended_no_respawn",
        SupervisorPhase::Terminal => "terminal",
    }
}

/// Converts to the local `Phase` (R10); [`phase_str`] stays for the wire mapping.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn local_phase(phase: sot_log::wire::SupervisorPhase) -> crate::workspaces::Phase {
    use crate::workspaces::Phase;
    use sot_log::wire::SupervisorPhase as SP;
    match phase {
        SP::Starting => Phase::Starting,
        SP::Ready => Phase::Ready,
        SP::Ending => Phase::Ending,
        SP::EndedNoRespawn => Phase::EndedNoRespawn,
        SP::Terminal => Phase::Terminal,
    }
}

/// ADR 0042 L1a (Codex review finding 6): the daemon's own watchdog
/// restart budget for a capsule supervisor — ADR 0041's own launcher
/// restart sequence ("restart with `--resume` on the launcher's shipped
/// 1/3/7/15/30 s sequence, at most 5 restarts in 60 s, then stop and
/// report"). The daemon has become that launcher for every capsule
/// workspace it creates or resumes, so this is the ADR's own row, not
/// new policy.
#[cfg_attr(not(windows), allow(dead_code))]
pub const RESTART_BACKOFFS: [std::time::Duration; 5] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(3),
    std::time::Duration::from_secs(7),
    std::time::Duration::from_secs(15),
    std::time::Duration::from_secs(30),
];
#[cfg_attr(not(windows), allow(dead_code))]
pub const MAX_RESTARTS_PER_WINDOW: usize = 5;
#[cfg_attr(not(windows), allow(dead_code))]
pub const RESTART_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// ADR 0042 L1a (Codex review findings 10/11): "a small semaphore over
/// spawns" — the SAME fixed-width bound reused for both the startup
/// resume-scan's concurrent spawns and `workspace.list`'s concurrent
/// lane queries, rather than two independently-invented numbers.
#[cfg_attr(not(windows), allow(dead_code))]
pub const LANE_CONCURRENCY: usize = 4;

/// Outcome of [`runtime::end_run`] — the daemon's own portable
/// vocabulary over `sot_log::supervisor_client::EndRunOutcome` (never
/// that raw, platform-specific type crossing into `handlers.rs`). Defined
/// here, outside `runtime`, so `handlers.rs`'s outcome→response
/// mapping stays plain and unit-testable on every platform; `end_run`'s
/// own real lane call is the only step gated to Windows and Linux only.
#[cfg_attr(not(windows), allow(dead_code))]
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
    /// (`FLAP_THRESHOLD`, `rust/log/src/supervisor.rs`), most often an
    /// agent argv that can never launch (e.g. `claude` missing from
    /// PATH). A `Terminal` authority admits no fresh `EndRun` anyway
    /// (`supervisor.rs`'s `handle_command` gates `EndRun` on
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
    /// ONLY once the SAME [`runtime::absence_proof`] `Unheld` uses has
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
    /// instant its last holder died — `sot_log::fence`) — AND
    /// [`runtime::leg_absent`] independently proved no LEG holds the
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
    /// row's lane (`runtime::is_definitely_orphaned` — decision 27's own
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

pub(crate) mod runtime {
    use super::{
        agent_argv, ActivationIntent,
        StartMode, FOREIGN_PHASE, LANE_CONCURRENCY, MAX_RESTARTS_PER_WINDOW,
        NEVER_STARTED_PHASE, RESTART_BACKOFFS, RESTART_WINDOW, UNREACHABLE_PHASE,
    };
    use crate::rows::spawn::detach::{sot_capsule_exe, spawn_detached_supervisor};
    use crate::workspaces::{StartPermit, Workspaces};
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::process::Child;

    /// `sot-capsule supervise`'s own clean-exit code (`EXIT_CLEAN`).
    const EXIT_CLEAN: i32 = 0;
    /// `sot-capsule supervise`'s own terminal-failure exit code
    /// (`EXIT_TERMINAL`) — unconditionally terminal to
    /// [`wait_and_classify`], never restarted (rule F, shrink round).
    const EXIT_TERMINAL: i32 = 69;
    /// `sot-capsule supervise`'s own fence-contention exit code
    /// (`sot_log::supervisor::EXIT_CONTENDED` — see that const's own doc
    /// for the full reasoning): the authority fence was already held by
    /// a LIVE supervisor. Distinct from [`EXIT_TERMINAL`] in
    /// [`wait_and_classify`] — NEVER a failure of this workspace's own
    /// run, only proof some other leg (almost always the previous
    /// authority for this SAME state dir, still finishing its own
    /// teardown) currently holds the fence.
    const EXIT_CONTENDED: i32 = 70;

    /// One status round trip: wire string plus the identity-carrying `Observation` it implies. BLOCKING.
    pub fn probe(state_dir: &Path) -> (&'static str, crate::workspaces::Observation) {
        use crate::workspaces::{Observation, SupervisorIdentity};
        if let Some(phase) =
            super::phase_for_missing_pointer(sot_log::pointer::pointer_path(state_dir).is_file())
        {
            return (phase, Observation::Stopped);
        }
        match sot_log::supervisor_client::query_status(state_dir) {
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

    /// Adopts the observation's supervisor as this row's epoch if it differs, then feeds it (guarded callers only).
    fn observe_with_adoption(ws: &crate::workspaces::Workspace, observation: crate::workspaces::Observation) {
        if let crate::workspaces::Observation::Phase { supervisor, .. } = &observation {
            if ws.current_supervisor() != Some(*supervisor) {
                ws.begin_supervisor_epoch(*supervisor);
            }
        }
        super::observer::observe(ws, observation);
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
        use sot_log::supervisor_client::EndRunOutcome as O;
        use sot_log::wire::SupervisorPhase;

        // A4b: the row's remembered scopes (`row_scope::SCOPES_FILE`)
        // are ended after the graceful end when a supervisor answers, and
        // before `Unheld` when none does, so no retry and no restarted
        // daemon counts the row ended while a scope may hold processes.
        #[cfg(target_os = "linux")]
        let (root, own) = (super::row_scope::root(), super::row_scope::own_rel().unwrap_or_default());
        #[cfg(not(target_os = "linux"))]
        let (root, own) = (PathBuf::new(), String::new());

        let (status, scope) = match sot_log::supervisor_client::query_status(state_dir) {
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
                // `supervisor.rs`'s `handle_command`). Skip the doomed
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
        // has set `authority.voyage_id` (`supervisor.rs`), so this is
        // always populated here.
        let voyage = status
            .voyage
            .expect("Ready/Ending implies a voyage_id (supervisor.rs's own recovery transition)");
        let outcome = sot_log::supervisor_client::end_run(state_dir, &voyage, reason)
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
            sot_log::fence::lock_supervisor(state_dir).map_err(|_| NotProven::FenceUnavailable)?;
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

    #[cfg(test)]
    mod is_definitely_orphaned_tests {
        use super::*;
        use sot_log::transport::TransportError;

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

    /// Whether a `lock_writer` failure is genuine contention (its OWN
    /// bounded-retry exhaustion, `fsutil.rs`) rather than some OTHER
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
        let voyage = match sot_log::pointer::validate(state_dir) {
            sot_log::pointer::PointerState::Valid(id) => id,
            other => return Err(format!("voyage pointer is not valid: {other:?}")),
        };
        let root = sot_log::supervisor::voyage_root_path(state_dir, &voyage);
        match sot_log::lock_writer(&root.join("writer.lock")) {
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
        if let Err(e) = sot_log::supervisor_client::stop(state_dir) {
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

    /// Spawn a capsule's supervisor authority AND hand it to a watchdog
    /// task together (ADR 0042 L1a, Codex review finding 6: "hand every
    /// spawned Child to a waiter task") — the daemon has become ADR
    /// 0041's own launcher for every capsule workspace it creates or
    /// resumes. Returns synchronously once the FIRST spawn attempt is
    /// known to have succeeded or failed, so a caller (`workspace.create`,
    /// finding 1) can roll back on a synchronous failure; the watchdog
    /// itself then runs entirely in the background.
    ///
    /// ADR 0043 decision 33: no claim to release any more — the CALLER
    /// holds this workspace's row guard (`Workspaces::capsule_guard`) for
    /// the whole spawn attempt (`start_supervisor`'s own doc), so nothing
    /// here needs to signal "the launch is no longer in flight" the way
    /// the old `starting` claim did.
    ///
    /// Order (supervisor-epoch ruling): spawn, SETTLE, adopt, install
    /// the watchdog — the settle moved inward by one frame from
    /// [`start_supervisor`], where it used to sit. Same 2s bound, same
    /// caller's guard, same total latency; what changes is that the
    /// window between spawn and first answered status now contains no
    /// watchdog at all, so no terminal mark and no second spawn can
    /// land inside it. The phase this settles to is what the caller
    /// reports.
    pub fn spawn_and_watch(
        permit: &StartPermit,
        sot_capsule_exe: &Path,
        state_dir: &Path,
        mode: StartMode,
        agent_argv: &[String],
        cwd: &Path,
        agent_name: &str,
        workspace_id: String,
        slug: String,
        workspaces: Workspaces,
    ) -> std::io::Result<&'static str> {
        // Accounts brief: resolved from the registry HERE, where every
        // spawn path (create, resume, start-on-attach) already converges
        // with both `workspace_id` and `workspaces` in hand -- rather than
        // threading two more scalars through every caller up the chain
        // (`start_supervisor`, `resume_locked`, `ensure_started`, …), which
        // never otherwise need to know an agent's KIND, only its
        // already-resolved argv. The pair is read at the moment of each
        // spawn and never captured, so the watchdog's own crash-restart
        // resolves it again rather than carrying this one (`workspace.
        // reauth` can move the account under a parked watchdog).
        // `unwrap_or_default` (kind "", account "") on a row gone by now
        // degrades to `account_env`'s own empty-account no-op below --
        // never worse than the row simply not existing.
        let (agent_kind, account) = workspaces
            .resolve(Some(&workspace_id))
            .map(|ws| (ws.agent(), ws.account()))
            .unwrap_or_default();
        let child = spawn_detached_supervisor(
            permit, sot_capsule_exe, state_dir, mode, agent_argv, cwd, agent_name, &workspace_id, &slug, &agent_kind, &account,
        )?;
        // The supervisor authors its own identity; this daemon only
        // LEARNS it, here, from the first status the settle draws out.
        // A settle that yields no `Phase` leaves the row unclaimed --
        // best-effort by design, since the background observer adopts
        // whenever the lane does answer.
        let (phase, observation) = settle_after_spawn(state_dir, &workspace_id);
        let identity = identity_of(&observation);
        if let Some(ws) = workspaces.resolve(Some(&workspace_id)) {
            observe_with_adoption(&ws, observation);
        }
        install_watchdog(
            workspace_id,
            sot_capsule_exe.to_path_buf(),
            state_dir.to_path_buf(),
            agent_argv.to_vec(),
            cwd.to_path_buf(),
            agent_name.to_string(),
            slug,
            child,
            identity,
            workspaces,
        );
        Ok(phase)
    }

    /// The spawn path shared by `workspace.create` (mode `Start` always — a
    /// brand new workspace has no state dir yet), `pty.open`'s
    /// start-on-attach ([`ensure_started`], mode picked by
    /// [`start_mode_for_phase`]), and `resume_all` (via [`resume_locked`]):
    /// locate `sot-capsule.exe` and spawn-and-watch it. ADR 0042 L1a Codex
    /// review finding 1's
    /// synchronous-failure contract applies to every caller: an `Err`
    /// here means no supervisor is running, and the caller must refuse
    /// its own op with this text rather than silently proceeding.
    ///
    /// Rule C (shrink round): does NOT create the state directory —
    /// `sot-capsule supervise` creates its own (`supervise_inner`'s first
    /// act, `rust/log/src/supervisor.rs`) once it actually runs, so a
    /// synchronous spawn failure here leaves nothing behind at all, not
    /// even an empty directory a later `phase_of` could misread.
    ///
    /// ADR 0043 decision 33: the CALLER holds this workspace's row guard
    /// (`Workspaces::capsule_guard`) for this whole call — every one does:
    /// `ensure_started` and `resume_if_absent`/`resume_locked` take it at
    /// their own entry, `workspace.create` and `resume_all` take it
    /// around their own call site (`handlers.rs`, this module's
    /// `resume_all`). With the guard already held by every caller, at
    /// most one spawn attempt per row can ever be in flight.
    ///
    /// Codex review (2026-09-11): establishes lane REACHABILITY before
    /// returning, not merely a successful spawn — [`settle_after_spawn`],
    /// still under the caller's own guard, now performed one frame
    /// inward by [`spawn_and_watch`] and simply handed back here. Every
    /// spawner converges on this ONE wait: fresh attach
    /// (`ensure_started`'s Start arm), a resume (`resume_locked`),
    /// `workspace.create`, and `resume_all` all call this function and
    /// get it for free. The watchdog's own
    /// restart is the one spawner that does NOT — it never installs a
    /// SECOND watchdog on top of its own loop, so it calls
    /// [`spawn_detached_supervisor`] directly and then
    /// [`settle_after_spawn`] itself, the same shared wait.
    pub fn start_supervisor(
        state_root: &Path,
        workspace_id: &str,
        mode: StartMode,
        agent_argv: &[String],
        project_root: &Path,
        agent_name: &str,
        slug: &str,
        workspaces: Workspaces,
    ) -> Result<&'static str, String> {
        // The run gate first, held to return: a refused start locates and
        // spawns nothing.
        let permit = workspaces.begin_start(workspace_id)?;
        let state_dir = super::state_dir_for(state_root, workspace_id);
        let exe = match sot_capsule_exe() {
            Ok(exe) => exe,
            Err(e) => return Err(format!("could not locate sot-capsule.exe next to this daemon: {e}")),
        };
        spawn_and_watch(
            &permit,
            &exe,
            &state_dir,
            mode,
            agent_argv,
            project_root,
            agent_name,
            workspace_id.to_string(),
            slug.to_string(),
            workspaces.clone(),
        )
        .map_err(|e| format!("capsule supervisor spawn failed: {e}"))
    }

    /// Mints a fresh voyage on the row's live authority — a run start, so
    /// it passes the gate first. The ONLY `supervisor_client::reset` call
    /// outside tests; the retire arm of [`ensure_started`] and
    /// `reauth::mint_replacement_voyage` both come through here.
    pub(crate) fn reset_run(workspaces: &Workspaces, workspace_id: &str, state_dir: &Path) -> Result<String, String> {
        let _permit = workspaces.begin_start(workspace_id)?;
        sot_log::supervisor_client::reset(state_dir).map_err(|e| e.to_string())
    }

    /// Bound for [`settle_after_spawn`] — the ONE deadline every spawn
    /// path shares (Codex review, 2026-09-11): fresh attach, boot resume,
    /// create, and the watchdog's own restart all wait this long, no
    /// more and no less, for a freshly spawned authority to become
    /// observable before the row's guard (held by every one of them for
    /// this whole wait) is released.
    const SPAWN_SETTLE_DEADLINE: Duration = Duration::from_secs(2);

    /// `SOT_TEST_SPAWN_SETTLE_MS` overrides [`SPAWN_SETTLE_DEADLINE`] for tests, read once per process (the
    /// `shutdown::shutdown_bound` convention); only tests set it. Every spawn path still shares it.
    fn spawn_settle_deadline() -> Duration {
        static OVERRIDE_MS: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        let override_ms =
            *OVERRIDE_MS.get_or_init(|| std::env::var("SOT_TEST_SPAWN_SETTLE_MS").ok().and_then(|s| s.parse().ok()));
        override_ms.map(Duration::from_millis).unwrap_or(SPAWN_SETTLE_DEADLINE)
    }

    /// Waits under the caller's guard for a spawn to settle, polling until [`spawn_settle_deadline`] (timeout WARNS). BLOCKING.
    fn settle_after_spawn(state_dir: &Path, workspace_id: &str) -> (&'static str, crate::workspaces::Observation) {
        let settle = spawn_settle_deadline();
        let starting_phase = super::phase_str(sot_log::wire::SupervisorPhase::Starting);
        let deadline = Instant::now() + settle;
        loop {
            let (phase, observation) = probe(state_dir);
            if phase != UNREACHABLE_PHASE && phase != starting_phase {
                return (phase, observation);
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    workspace_id = %workspace_id, phase, deadline = ?settle,
                    "capsule workspace: lane did not settle within the post-spawn deadline"
                );
                return (phase, observation);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether a capsule workspace's supervisor needs starting given an
    /// ALREADY-PROBED `phase` — no I/O itself (switch-latency Phase 1:
    /// the healthy already-running path used to call [`phase_of`] a
    /// SECOND time here; reusing an already-fetched phase string costs
    /// nothing beyond what an already-answered lane already told the
    /// caller). [`NEVER_STARTED_PHASE`] (no published pointer) means
    /// `Start`; [`UNREACHABLE_PHASE`] means `Resume`; any answered
    /// lifecycle phase means a supervisor is already up — `None`.
    fn start_mode_for_phase(phase: &str) -> Option<StartMode> {
        match phase {
            NEVER_STARTED_PHASE => Some(StartMode::Start),
            UNREACHABLE_PHASE => Some(StartMode::Resume),
            _ => None,
        }
    }

    /// Invariant (Codex review round 6/7 BLOCKERs): EVERY decision point
    /// -- the initial probe AND the phase a spawn THIS call itself just
    /// made settles at -- acts ONLY on a genuinely RESTING phase:
    /// `ready`, `ended_no_respawn`, `terminal`, never-started
    /// (`stopped`), `foreign` (a peer identity check already resolved
    /// it, same as before this lane), or `unreachable` with NO live
    /// watchdog (nothing else will ever act on it, so this caller must).
    /// Every OTHER phase — `starting`, `ending`, or `unreachable` while a
    /// watchdog owns it — is TRANSIENT: the row is mid-flight to
    /// somewhere else, and deciding from a snapshot of it is exactly the
    /// bug this closes (round 6: a Selection reading a fleeting
    /// `starting` after the watchdog's own settle gave up; round 7: the
    /// SAME snapshot read straight from this call's OWN spawn, with no
    /// watchdog involved at all — an adopted row's dead supervisor,
    /// `--resume`d fresh, can report `starting` through its own recovery
    /// for many seconds). `ensure_started`'s own loop is what waits out
    /// a transient phase; this function only tells the two apart.
    fn is_resting_phase(phase: &str, watchdog_owns_it: bool) -> bool {
        phase == NEVER_STARTED_PHASE
            || phase == FOREIGN_PHASE
            || phase == UNREACHABLE_PHASE && !watchdog_owns_it
            || phase == super::phase_str(sot_log::wire::SupervisorPhase::Ready)
            || phase == super::phase_str(sot_log::wire::SupervisorPhase::EndedNoRespawn)
            || phase == super::phase_str(sot_log::wire::SupervisorPhase::Terminal)
    }

    #[cfg(test)]
    mod is_resting_phase_tests {
        use super::*;

        #[test]
        fn resting_phases_never_wait_regardless_of_watchdog() {
            for phase in [NEVER_STARTED_PHASE, FOREIGN_PHASE, "ready", "ended_no_respawn", "terminal"] {
                assert!(is_resting_phase(phase, true), "{phase} must be resting with a watchdog");
                assert!(is_resting_phase(phase, false), "{phase} must be resting with no watchdog");
            }
        }

        #[test]
        fn unreachable_rests_only_with_no_watchdog() {
            assert!(is_resting_phase(UNREACHABLE_PHASE, false), "nobody else will ever act -- this caller must");
            assert!(!is_resting_phase(UNREACHABLE_PHASE, true), "a live watchdog owns the restart -- wait for it");
        }

        #[test]
        fn transient_phases_always_wait_even_with_no_watchdog() {
            // Codex review round 6/7 BLOCKERs: a settle timeout (the
            // watchdog's own, OR this call's own fresh spawn) can return
            // "starting" -- this must NEVER be treated as a decision
            // point, watchdog or not (an adopted authority with no
            // watchdog at all is still mid-flight here too).
            for phase in ["starting", "ending"] {
                assert!(!is_resting_phase(phase, true), "{phase} is transient even with a watchdog");
                assert!(!is_resting_phase(phase, false), "{phase} is transient even with no watchdog");
            }
        }
    }

    /// The guard-HELD body shared by [`resume_if_absent`] (which takes
    /// the row's guard itself, around this whole call), [`ensure_started`]
    /// (which already holds it for its own whole call, on the
    /// `UNREACHABLE_PHASE` arm), and `handlers.rs`'s
    /// `destroy_capsule_workspace` (which holds the SAME row guard
    /// across this call and its own following `end_run`, so it must
    /// reach this guard-free inner directly rather than through
    /// [`resume_if_absent`] — a second `blocking_lock` on a guard this
    /// caller already holds would deadlock) — ADR 0043 decision 33.
    /// `pub` for that cross-module reach; still crate-internal in effect
    /// (`mod runtime` itself is private, re-exported only within this
    /// crate via `capsule_workspace`'s own `pub use runtime::*`).
    /// Rechecks, now that the guard is actually held: the row is still
    /// registered (`Err` — "unknown workspace" — a concurrent remover
    /// could have removed it while this call waited for the lock); if
    /// the watchdog already observed it `Phase::Terminal` (latched), reports that phase without touching the lane
    /// again. Otherwise probes once: any phase OTHER than
    /// [`UNREACHABLE_PHASE`] is returned as-is — nothing to resume (in
    /// particular, a missing state dir reads `NEVER_STARTED_PHASE` here
    /// and this returns WITHOUT ever spawning — never a licence to
    /// recreate one). Only a genuinely unreachable lane spawns, and only
    /// with `StartMode::Resume` — this is the resume path, never the
    /// create one — via [`start_supervisor`], which settles before
    /// returning (`Ok`) or reports why it could not spawn at all
    /// (`Err`). Never sends `reset`: an `EndedNoRespawn` settle is
    /// reported as-is here — retiring it is attach's own job (R3), not
    /// resume's.
    pub fn resume_locked(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        workspaces: Workspaces,
    ) -> Result<&'static str, String> {
        let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
            return Err("unknown workspace".to_string());
        };
        if ws.phase() == crate::workspaces::Phase::Terminal {
            return Ok(super::phase_str(sot_log::wire::SupervisorPhase::Terminal));
        }
        let state_dir = super::state_dir_for(state_root, workspace_id);
        let (phase, observation) = probe(&state_dir);
        if phase != UNREACHABLE_PHASE {
            // Already answering -- no spawn, but still an adoption if this row had none yet.
            observe_with_adoption(&ws, observation);
            return Ok(phase);
        }
        // Ruling: the watchdog is the single writer of restarts for a
        // child this daemon spawned. A row whose watchdog is still alive
        // reports its own (unreachable) phase as-is -- the caller waits
        // as it would for Starting -- rather than racing a second spawn
        // in front of the watchdog's own backoff and restart budget. A
        // row with no watchdog (never started, terminal, or a live
        // authority merely ADOPTED at boot) reaches the spawn below
        // unchanged.
        if ws.watchdog_owner().is_some() {
            return Ok(phase);
        }
        let argv = agent_argv(agent_kind, Some(project_root))?;
        start_supervisor(state_root, workspace_id, StartMode::Resume, &argv, project_root, agent_name, slug, workspaces)
    }

    /// R2's own resume path (ADR 0043 decision 33): a row whose lane has
    /// gone quiet is resumed, in place, by the operation that needed it
    /// live — never by a list. BLOCKING (`phase_of`, `query_status`, and
    /// — when it actually resumes — a process spawn are all real I/O):
    /// callers run it via `spawn_blocking`. Takes this workspace's OWN
    /// guard (`Workspaces::capsule_guard`) for its whole duration —
    /// `Err("unknown workspace")` if the row is not currently registered
    /// (that call itself refuses to mint an orphan guard entry, Codex
    /// review 2026-09-11 — no lock to even take); [`resume_locked`]'s own
    /// doc has what the SECOND recheck, once the lock is actually held,
    /// covers. Headless callers (`handlers.rs`'s `pty.input`/
    /// `pty.screen`) use this in place of a bare [`phase_of`] read so a
    /// row whose supervisor died between two ops resumes itself rather
    /// than answering `NotReady` forever; `pty.open`'s own attach path
    /// keeps using [`ensure_started`] instead — it needs the `Start` arm
    /// this function deliberately does not have (resume-only intent: it
    /// never starts a row that has no pointer published at all, and it
    /// never sends `reset`).
    pub fn resume_if_absent(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        workspaces: Workspaces,
    ) -> Result<&'static str, String> {
        let Some(guard) = workspaces.capsule_guard(workspace_id) else {
            return Err("unknown workspace".to_string());
        };
        let _held = guard.blocking_lock();
        resume_locked(state_root, workspace_id, agent_kind, agent_name, slug, project_root, workspaces)
    }

    /// Bounds how many re-probe PASSES [`ensure_started`] spends waiting
    /// for a transient phase to settle (a watchdog mid-restart; or
    /// simply a still-starting/still-recovering authority, watchdog or
    /// not) before falling back to reporting the row's phase unacted-on,
    /// exactly as it did before that wait existed. A pass count, not
    /// wall-clock time (round 9: a wall-clock deadline, even one started
    /// at the first `WaitForSettle`, still ticks down while this call
    /// merely waits to ACQUIRE the guard -- behind a watchdog's own
    /// 7/15/30s backoff, say -- time that was never this budget's to
    /// spend either). NOT a tight bound in wall-clock terms: a pass that
    /// re-enters the retire arm pays a `stop` plus a fresh
    /// `SPAWN_SETTLE_DEADLINE` (2s) each time, so 50 passes can hold the
    /// row's guard for minutes on a row whose recovery keeps outlasting
    /// that deadline -- accepted rather than a second counter, since the
    /// fallback is always the honest "still unacted-on" report this
    /// function already gives, never a wrong decision.
    const ACTIVATION_MAX_REPROBES: u32 = 50;

    /// How often [`ensure_started`] re-probes a transient phase while
    /// waiting. No progress signal to race against it (Codex review
    /// round 8: a `Notify` here saved at most one interval's worth of
    /// latency, never correctness, and cost a real subscription-ordering
    /// hazard to close properly -- deleted).
    const ACTIVATION_REPROBE_INTERVAL: Duration = Duration::from_millis(200);

    /// The ONE shared activation boundary for every caller: guard, inert-anchor refusal, then start/resume/(Selection-only) retire+reset. BLOCKING.
    pub fn ensure_started(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        intent: ActivationIntent,
        workspaces: Workspaces,
    ) -> Result<Option<()>, String> {
        let Some(guard) = workspaces.capsule_guard(workspace_id) else {
            return Err("unknown workspace".to_string());
        };
        let mut reprobes: u32 = 0;
        // Round-9 BLOCKER: the identity of the fresh authority THIS
        // activation itself spawned to retire an ended row, carried
        // across passes -- see `ensure_started_locked`'s own doc for why.
        let mut own_spawn: Option<crate::workspaces::SupervisorIdentity> = None;
        loop {
            let held = guard.blocking_lock();
            // Rechecked under the guard -- a concurrent remover could have removed the row while this call waited.
            let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
                return Err("unknown workspace".to_string());
            };
            // R1: the ONE place this check lives, under the guard against the CURRENT row.
            if workspaces.is_inert_default_anchor(&ws) {
                return Ok(None);
            }
            // Clear-on-attempt happens HERE inside the guard so two serialized attempts can't interleave.
            ws.set_activation_error(None);
            match ensure_started_locked(
                state_root, workspace_id, agent_kind, agent_name, slug, project_root, intent, workspaces.clone(),
                &mut own_spawn,
            ) {
                LockedStep::Done(result) => {
                    if let Err(detail) = &result {
                        ws.set_activation_error(Some(detail.clone()));
                    }
                    return result;
                }
                // Ruling: never drop the caller's intent, and never
                // decide from a transient phase (see `is_resting_phase`).
                // Release the guard (a watchdog, or the authority's own
                // recovery, needs it free to make ANY progress) and
                // sleep one re-probe interval, then re-acquire and
                // re-run this WHOLE decision from scratch with the SAME
                // original intent -- so a Selection on a run that comes
                // back EndedNoRespawn still retires and resets, never
                // silently "succeeds" on a stale snapshot mid-transition.
                LockedStep::WaitForSettle => {
                    // Test-only: one marker per reprobe cycle, so a test
                    // can wait for (or count) this loop's own progress
                    // instead of guessing a sleep duration. No-op unless
                    // `SOT_TEST_ACTIVATION_BARRIER` is set.
                    crate::server::record_test_activation_marker("waitforsettle");
                    reprobes += 1;
                    if reprobes > ACTIVATION_MAX_REPROBES {
                        // The budget is spent: report the phase as the
                        // pre-ruling code always did for this exact
                        // situation -- attempted, still unsettled, spawn
                        // deferred to whatever is already in flight.
                        // Pre-existing, filed for later (round 10 record):
                        // this `Ok(Some(()))` is indistinguishable on the
                        // wire from a genuine success, so `pty.open`
                        // answers `attach_direct` onto a row that may
                        // still be `ended_no_respawn`, with no
                        // `activation_error` set -- exhaustion itself is
                        // not surfaced as a caller-visible failure.
                        return Ok(Some(()));
                    }
                    drop(held);
                    // BLOCKING (this whole function is): plain sleep, no
                    // tokio runtime handle needed, same as
                    // `settle_after_spawn`'s own wait.
                    std::thread::sleep(ACTIVATION_REPROBE_INTERVAL);
                }
            }
        }
    }

    /// What one guard-held attempt at [`ensure_started_locked`] concluded.
    enum LockedStep {
        /// Genuinely resolved -- nothing further to do differently.
        Done(Result<Option<()>, String>),
        /// The row's phase is transient, not a resting point a decision
        /// may be made from -- see [`is_resting_phase`]'s own doc for
        /// the invariant, and [`ensure_started`]'s own loop for the wait.
        WaitForSettle,
    }

    /// [`ensure_started`]'s guard-held body, after membership/inert-anchor/activation-error clear.
    ///
    /// `own_spawn` is [`ensure_started`]'s own local, carried across
    /// `WaitForSettle` passes (round-9 BLOCKER): the identity of the
    /// fresh authority a PRIOR pass of THIS SAME activation spawned to
    /// retire an ended row. Without it, a transient `retired_phase`
    /// below returns `WaitForSettle`, the next pass re-runs this whole
    /// function from scratch, sees `ended_no_respawn` again, and (with
    /// no memory of the spawn it just did) retires AGAIN -- stopping the
    /// authority this same activation only just spawned and spawning
    /// another. A row whose `--resume` recovery takes longer than
    /// `SPAWN_SETTLE_DEADLINE` on every attempt then cycles stop, spawn,
    /// settle, wait, stop forever: no `reset` is ever reached, so the
    /// row can never start a new run. Recognizing "the current resident
    /// IS the fresh binary I already spawned" breaks that cycle: reset
    /// it directly, no second stop, no second spawn.
    fn ensure_started_locked(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        intent: ActivationIntent,
        workspaces: Workspaces,
        own_spawn: &mut Option<crate::workspaces::SupervisorIdentity>,
    ) -> LockedStep {
        let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
            return LockedStep::Done(Err("unknown workspace".to_string()));
        };
        let state_dir = super::state_dir_for(state_root, workspace_id);
        let (initial_phase, initial_observation) = probe(&state_dir);
        observe_with_adoption(&ws, initial_observation);
        // Ruling: never drop the caller's intent, and never decide from
        // a transient snapshot -- see `is_resting_phase`'s own doc for
        // the invariant, and `ensure_started`'s own loop for what
        // happens on `WaitForSettle` (this checks BEFORE computing
        // `mode`: `start_mode_for_phase` already maps a transient
        // `starting` read to `None`, "nothing to do", which is exactly
        // the stale-snapshot bug this closes).
        if !is_resting_phase(initial_phase, ws.watchdog_owner().is_some()) {
            return LockedStep::WaitForSettle;
        }
        // R4c: Reconnect permits only StartMode::Resume -- never a row's first-ever start.
        let mode = start_mode_for_phase(initial_phase);
        let mode = if intent == ActivationIntent::Reconnect && mode != Some(StartMode::Resume) { None } else { mode };
        let (spawned, settled_phase) = match mode {
            Some(StartMode::Start) => {
                let argv = match agent_argv(agent_kind, Some(project_root)) {
                    Ok(a) => a,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                let phase = match start_supervisor(
                    state_root, workspace_id, StartMode::Start, &argv, project_root, agent_name, slug, workspaces.clone(),
                ) {
                    Ok(p) => p,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                (Some(()), phase)
            }
            Some(StartMode::Resume) => {
                let phase = match resume_locked(
                    state_root, workspace_id, agent_kind, agent_name, slug, project_root, workspaces.clone(),
                ) {
                    Ok(p) => p,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                (Some(()), phase)
            }
            None => (None, initial_phase),
        };
        // Ruling (round 7 BLOCKER): the phase a spawn THIS call itself
        // just made settles at is EXACTLY as liable to be transient as
        // the initial probe was -- a watchdog now owns the fresh child
        // either way (`Start`/`Resume` both install one via
        // `spawn_and_watch`), so only "resting or not" is left to ask.
        // `mode == None` means `settled_phase == initial_phase`, already
        // proven resting above -- asking again is cheap and uniform.
        // Round 10: NOT gated to Selection -- a round-9 attempt to skip
        // this wait for Reconnect broke a slow `--resume` recovery:
        // `settle_after_spawn` reads `unreachable` (the fresh child not
        // listening yet) past its own 2s deadline just as readily as
        // `starting`, so an ungated Reconnect returned `Ok` at once onto
        // a row with nothing actually resumed yet, and the bridge's next
        // connect failed `lane_absent` instead of converging -- the exact
        // scenario case 4 below exercises. Both intents wait.
        if !is_resting_phase(settled_phase, true) {
            return LockedStep::WaitForSettle;
        }
        // One answered phase still has no live leg to attach to:
        // `EndedNoRespawn` (`--resume`/`--start` deliberately never
        // resurrect it — ADR 0041's own no-resurrection rule). A new run
        // never starts on a resident authority; replacement requires a
        // confirmed stop (ADR 0043 decision 33's retirement clause).
        let ended_phase = super::phase_str(sot_log::wire::SupervisorPhase::EndedNoRespawn);
        if settled_phase == ended_phase {
            // RB: a passive Reconnect never resets an ended run -- only a real Selection may retire+reset it below.
            if intent == ActivationIntent::Reconnect {
                return LockedStep::Done(Ok(spawned));
            }
            // Round-9 BLOCKER fast path: a PRIOR pass of this SAME
            // activation already retired this row (see `own_spawn`'s own
            // doc above) and the resident authority is STILL that exact
            // fresh spawn -- reset it directly, no second stop, no
            // second spawn. Without this, a transient `retired_phase`
            // below sends this function back to `WaitForSettle`, and the
            // NEXT pass re-enters this exact branch from scratch with no
            // memory of the spawn it just made, stopping and respawning
            // AGAIN -- a row whose recovery consistently outlasts
            // `SPAWN_SETTLE_DEADLINE` would then never converge.
            let already_fresh = own_spawn.is_some_and(|identity| ws.current_supervisor() == Some(identity));
            if !already_fresh {
                // Retire the resting authority (attach's own job, not
                // resume's) before minting a new run over it: the WAITING
                // `stop` (confirmed exit, never `stop_and_warn`) first -- an
                // `Err` here leaves the row untouched, nothing replaced --
                // then a fresh spawn via the same guarded resume body every
                // other caller uses. Sending `reset` straight to the OLD
                // resident process (the prior behaviour) would let IT mint
                // the new voyage and spawn the new leg from whatever binary
                // it cached at its own start.
                if let Err(e) = sot_log::supervisor_client::stop(&state_dir) {
                    return LockedStep::Done(Err(format!("capsule workspace retire (stop before reset) failed: {e}")));
                }
                let argv = match agent_argv(agent_kind, Some(project_root)) {
                    Ok(a) => a,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                let retired_phase = match start_supervisor(
                    state_root, workspace_id, StartMode::Resume, &argv, project_root, agent_name, slug, workspaces.clone(),
                ) {
                    Ok(p) => p,
                    Err(e) => return LockedStep::Done(Err(e)),
                };
                // Remember which authority THIS pass just spawned (a
                // fresh probe, not `retired_phase` alone, since that
                // carries no identity) so a LATER pass -- if this one's
                // own settle below finds it still transient -- recognizes
                // its own child instead of retiring it all over again.
                // Always OVERWRITES, never merely sets: a probe that
                // comes back anything other than `Phase` (the spawn died
                // before this could even read it, say) must CLEAR the
                // old identity too, or a later pass could wrongly credit
                // this attempt with a PRIOR pass's now-dead spawn.
                use crate::workspaces::Observation;
                *own_spawn = match probe(&state_dir) {
                    (_, Observation::Phase { supervisor, .. }) => Some(supervisor),
                    _ => None,
                };
                // Ruling (round 8): the SAME rule applies to the retire
                // arm's own respawn -- a transient `retired_phase` (this
                // fresh authority still recovering) is not yet the
                // honest "it settled somewhere other than
                // ended_no_respawn" failure reported below; wait for it
                // to actually rest first, same as every other decision
                // point in this function.
                if !is_resting_phase(retired_phase, true) {
                    return LockedStep::WaitForSettle;
                }
                if retired_phase != ended_phase {
                    return LockedStep::Done(Err(format!(
                        "capsule workspace retire: resumed authority settled to {retired_phase} instead of ended_no_respawn"
                    )));
                }
            }
            // The second of the two resets in this tree, and the other is
            // `reauth::mint_replacement_voyage`; both go through
            // [`reset_run`]. Its doc comment carries the long form.
            return LockedStep::Done(match reset_run(&workspaces, workspace_id, &state_dir) {
                // Mints a fresh voyage on the SAME epoch; the observer's next round supersedes the latch.
                Ok(_new_voyage) => Ok(Some(())),
                Err(e) => Err(format!("capsule workspace reset (after retiring an ended run) failed: {e}")),
            });
        }
        LockedStep::Done(Ok(spawned))
    }

    /// What one leg's exit means for the watchdog's own decision —
    /// ADR 0042 L1a, Codex review finding 6; rule F (shrink round)
    /// simplified this from three outcomes to two.
    enum LegOutcome {
        /// Exit 0 (`EXIT_CLEAN`): the run ended normally. Never
        /// restarted — the lane (or its absence) already says
        /// everything a client needs.
        Clean,
        /// Exit 69 (`EXIT_TERMINAL`): terminal, UNCONDITIONALLY — never
        /// restarted, regardless of whether the lane still answers. Rule
        /// F: the OLD "does the lane still answer" discriminator (a
        /// dropped `ForeignFence` outcome) tried to tell apart "lost the
        /// race for `supervisor.lock`" from "a genuinely exhausted
        /// producer", but `sot-capsule supervise` already runs its OWN
        /// internal flap/retry budget (`FLAP_THRESHOLD`,
        /// `respawn_or_terminal` in `rust/log/src/supervisor.rs`) before
        /// it ever chooses to exit 69 — so a second restart layer on top,
        /// here, is always redundant at best. At worst it actively hid a
        /// real failure: a producer that will NEVER recover (e.g.
        /// `claude` missing from the daemon's PATH) burned the WHOLE
        /// daemon-side restart budget (`MAX_RESTARTS_PER_WINDOW` attempts
        /// against `RESTART_WINDOW`) before finally reaching this same
        /// terminal mark anyway — "the supervisor's own three legs, not a
        /// rolling restart loop."
        Terminal,
        /// Exit 70 (`EXIT_CONTENDED`): the authority fence was already
        /// held by a LIVE supervisor when this leg tried to acquire it —
        /// almost always the previous authority for this SAME state dir,
        /// still finishing its own teardown. NEVER treated as
        /// [`Terminal`] (that would mark a perfectly healthy workspace
        /// terminal out from under a run some OTHER leg is still
        /// actively serving). ADR 0043 decision 33 (shrink round): no
        /// longer re-probed for adoption either — [`install_watchdog`]
        /// logs and returns, leaving the row for the next attach's own
        /// [`resume_if_absent`]/[`ensure_started`] to find and resume
        /// under the row's guard, same as any other quiet lane.
        Contended,
        /// Anything else: a genuine crash needing the restart sequence.
        Crash,
    }

    /// Maps a confirmed (or absent) exit code to the watchdog's own
    /// outcome vocabulary.
    fn classify_exit_code(code: Option<i32>) -> LegOutcome {
        match code {
            Some(EXIT_CLEAN) => LegOutcome::Clean,
            Some(EXIT_TERMINAL) => LegOutcome::Terminal,
            Some(EXIT_CONTENDED) => LegOutcome::Contended,
            _ => LegOutcome::Crash,
        }
    }

    /// Waits for `child` to end and classifies the result.
    /// `tokio::process::Child::wait` is trusted outright: the daemon is
    /// the sole, unambiguous owner of a supervisor it spawned itself —
    /// ADR 0043 decision 33, "a watchdog exists only for a `Child` the
    /// daemon launched." There is no adopted twin any more: an authority
    /// `resume_all` merely finds already alive at boot is never watched
    /// at all (see that function's own doc); if it later goes quiet, the
    /// next attach's `resume_if_absent`/`ensure_started` spawns and
    /// watches a FRESH leg, which this function then does own.
    async fn wait_and_classify(mut child: Child, workspace_id: &str) -> LegOutcome {
        let code = match child.wait().await {
            Ok(status) => status.code(),
            Err(e) => {
                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: wait() failed; treating as a crash");
                return LegOutcome::Crash;
            }
        };
        classify_exit_code(code)
    }

    /// Whether it is still THIS watchdog's business to act on
    /// `workspace_id`, checked under the row's own guard (the caller
    /// proves it by already holding it) before EITHER mutation the
    /// watchdog can make: a restart, or a terminal mark. Rechecks, now
    /// that the guard is actually held, that the row is still
    /// registered, not already marked terminal, AND still genuinely
    /// unreachable (`phase_of`). The window this closes is real, not
    /// merely theoretical: `wait_and_classify` itself takes no lock, so
    /// between a leg's confirmed exit and this watchdog's own task
    /// actually reaching the guard, a stale attach's own
    /// `ensure_started`/`resume_if_absent` (on a separate blocking-pool
    /// thread — genuinely concurrent with this async task on a
    /// multi-worker runtime) can win the guard FIRST and resume the row
    /// itself, installing its own fresh watchdog. Any of the three false
    /// means some OTHER actor already settled this row's fate while this
    /// watchdog waited — a restart would then spawn a REDUNDANT
    /// authority, and a terminal mark would misreport a row a fresh
    /// authority is already serving. BLOCKING (`phase_of`): callers run
    /// it via `spawn_blocking`.
    fn watchdog_may_act(workspace_id: &str, state_dir: &Path, workspaces: &Workspaces) -> bool {
        let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
            tracing::debug!(workspace_id = %workspace_id, "capsule supervisor watchdog: row no longer registered; stopping");
            return false;
        };
        if ws.phase() == crate::workspaces::Phase::Terminal {
            return false;
        }
        if phase_of(state_dir) != UNREACHABLE_PHASE {
            tracing::debug!(
                workspace_id = %workspace_id,
                "capsule supervisor watchdog: row was already resumed by another actor while this watchdog waited for the guard; not acting"
            );
            return false;
        }
        true
    }

    /// The identity a settle learned, if its lane answered at all — the
    /// ONE place a daemon-spawned supervisor's identity now comes from
    /// (supervisor-epoch ruling: the supervisor authors it, this daemon
    /// only learns it over the lane, on every platform). Every caller
    /// OVERWRITES with this, never merely sets: a settle that came back
    /// anything other than `Phase` must clear the previous leg's
    /// identity too, or a later terminal mark could be credited to a
    /// prior, now-dead spawn.
    fn identity_of(observation: &crate::workspaces::Observation) -> Option<crate::workspaces::SupervisorIdentity> {
        match observation {
            crate::workspaces::Observation::Phase { supervisor, .. } => Some(*supervisor),
            _ => None,
        }
    }

    /// Mints an ownership token for one watchdog install — unique for
    /// this daemon's lifetime, which is the whole guarantee
    /// `Workspace::watchdog_owner` needs (see that field's own doc).
    fn next_watchdog_owner() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// A watchdog's exit-classification observation; no guard needed.
    /// `Some` judges the mark against the identity the leg's own settle
    /// learned, exactly as before. `None` — a leg that exited before
    /// ever answering its lane, which `sot-capsule supervise` does on
    /// every bootstrap failure (it returns `EXIT_TERMINAL` from three
    /// sites ahead of its own accept loop) — marks
    /// [`crate::workspaces::Observation::TerminalUnclaimed`] instead, so
    /// the row still latches `terminal` rather than reading `stopped`
    /// and re-spawning the same instant failure on every attach.
    fn observe_terminal(workspaces: &Workspaces, workspace_id: &str, identity: Option<crate::workspaces::SupervisorIdentity>) {
        if let Some(ws) = workspaces.resolve(Some(workspace_id)) {
            let observation = match identity {
                Some(supervisor) => {
                    crate::workspaces::Observation::Phase { phase: crate::workspaces::Phase::Terminal, supervisor, voyage: None }
                }
                None => crate::workspaces::Observation::TerminalUnclaimed,
            };
            super::observer::observe(&ws, observation);
        }
    }

    /// Test-only barrier at the top of the watchdog's own Crash-arm
    /// restart attempt, BEFORE it ever takes the row's guard: when
    /// `SOT_TEST_ACTIVATION_BARRIER` is set, blocks until the test
    /// creates `<that path>.watchdog-restart` -- a file SEPARATE from
    /// the main barrier, so a test can hold the watchdog and
    /// `pty.open`'s own activation independently and so prove either
    /// lock ordering deterministically (Codex review round 6 SHOULD-FIX:
    /// replace an uncontrolled race with exactly this). No-op in
    /// production; gives up past a generous bound rather than hang a
    /// forgotten release forever.
    async fn wait_for_test_watchdog_restart_barrier() {
        let Ok(barrier_path) = std::env::var("SOT_TEST_ACTIVATION_BARRIER") else {
            return;
        };
        let path = std::path::PathBuf::from(format!("{barrier_path}.watchdog-restart"));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while !path.is_file() {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(path = ?path, "watchdog restart test barrier: released by timeout, not by the test");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// The watchdog itself: waits for the leg to exit, classifies it, and
    /// on a crash restarts with `--resume` under ADR 0041's own launcher
    /// restart sequence (`RESTART_BACKOFFS`, at most `MAX_RESTARTS_PER_
    /// WINDOW` within `RESTART_WINDOW`), then observes the workspace `Terminal` (latched — `workspace.list` reads
    /// it from memory, rule F). A `Contended` leg (decision 33) logs and returns outright.
    ///
    /// ADR 0043 decision 33: "a watchdog exists only for a `Child` the
    /// daemon launched" — `child` starts as [`spawn_and_watch`]'s own
    /// freshly-spawned process, and every SUBSEQUENT leg (a crash
    /// restart) is a fresh spawn too. There is no adopted counterpart —
    /// see `resume_all`'s own doc for what an already-alive authority
    /// gets instead (nothing, until it goes quiet and a fresh attach
    /// resumes and watches it). `None` from [`Workspaces::capsule_guard`]
    /// at entry (the row already gone by the time this task got to ask)
    /// means there is nothing to watch at all.
    ///
    /// R4a: `Terminal` reports immediately, no guard, identity-judged; `Crash` holds the guard across recheck/backoff/spawn.
    fn install_watchdog(
        workspace_id: String,
        sot_capsule_exe: PathBuf,
        state_dir: PathBuf,
        argv: Vec<String>,
        cwd: PathBuf,
        agent_name: String,
        slug: String,
        child: Child,
        initial_identity: Option<crate::workspaces::SupervisorIdentity>,
        workspaces: Workspaces,
    ) {
        tokio::spawn(async move {
            let Some(capsule_guard) = workspaces.capsule_guard(&workspace_id) else {
                return;
            };
            // Ruling: the watchdog is the single writer of restarts for a
            // child this daemon spawned. This guard is the ONE place the
            // row's `watchdog_owner` fact is announced (constructor) and
            // retracted (Drop) -- a compare-and-clear against THIS
            // task's own token, never a plain clear, so a superseded
            // watchdog's belated cleanup can never erase a replacement's
            // ownership set after it. One token per task, minted once:
            // ownership is a property of the WATCHDOG, not of whichever
            // leg it currently holds, so a respawn has nothing to
            // announce here.
            struct WatchdogOwnerGuard {
                workspaces: Workspaces,
                workspace_id: String,
                token: u64,
            }
            impl WatchdogOwnerGuard {
                fn new(workspaces: Workspaces, workspace_id: String) -> Self {
                    let token = next_watchdog_owner();
                    if let Some(ws) = workspaces.resolve(Some(&workspace_id)) {
                        ws.set_watchdog_owner(token);
                    }
                    Self { workspaces, workspace_id, token }
                }
            }
            impl Drop for WatchdogOwnerGuard {
                fn drop(&mut self) {
                    if let Some(ws) = self.workspaces.resolve(Some(&self.workspace_id)) {
                        ws.clear_watchdog_owner_if(self.token);
                    }
                }
            }
            let _watchdog_owner_guard = WatchdogOwnerGuard::new(workspaces.clone(), workspace_id.clone());
            // What each exit classification is judged against: the
            // identity the CURRENT leg's own settle learned, or `None`
            // when its lane never answered.
            let mut current_identity = initial_identity;
            let mut leg_opt = Some(child);
            let mut restart_times: Vec<Instant> = Vec::new();
            loop {
                let outcome = match leg_opt.take() {
                    Some(c) => wait_and_classify(c, &workspace_id).await,
                    // A previous restart attempt itself found nothing to
                    // wait on -- counts as another crash against the
                    // same budget.
                    None => LegOutcome::Crash,
                };
                match outcome {
                    LegOutcome::Clean => return,
                    LegOutcome::Terminal => {
                        tracing::warn!(
                            workspace_id = %workspace_id,
                            "capsule supervisor watchdog: leg exited terminal (69) -- marking terminal, no restart"
                        );
                        observe_terminal(&workspaces, &workspace_id, current_identity);
                        return;
                    }
                    LegOutcome::Contended => {
                        tracing::info!(
                            workspace_id = %workspace_id,
                            "capsule supervisor watchdog: leg exited contended (70) -- another authority holds the fence; leaving the row for the next attach"
                        );
                        return;
                    }
                    LegOutcome::Crash => {
                        // Decided before taking the guard -- giving up needs no recheck (R4a).
                        let now = Instant::now();
                        restart_times.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                        if restart_times.len() >= MAX_RESTARTS_PER_WINDOW {
                            tracing::error!(
                                workspace_id = %workspace_id, window = ?RESTART_WINDOW, max = MAX_RESTARTS_PER_WINDOW,
                                "capsule supervisor watchdog: restart budget exhausted -- giving up, marking terminal"
                            );
                            observe_terminal(&workspaces, &workspace_id, current_identity);
                            return;
                        }
                        wait_for_test_watchdog_restart_barrier().await;
                        let _held = capsule_guard.lock().await;
                        let may_act = {
                            let dir = state_dir.clone();
                            let wsid = workspace_id.clone();
                            let workspaces = workspaces.clone();
                            tokio::task::spawn_blocking(move || watchdog_may_act(&wsid, &dir, &workspaces))
                                .await
                                .unwrap_or(false)
                        };
                        if !may_act {
                            return;
                        }
                        let backoff = RESTART_BACKOFFS[restart_times.len().min(RESTART_BACKOFFS.len() - 1)];
                        tracing::warn!(
                            workspace_id = %workspace_id, backoff = ?backoff, attempt = restart_times.len() + 1,
                            "capsule supervisor watchdog: crashed, restarting with --resume"
                        );
                        tokio::time::sleep(backoff).await;
                        restart_times.push(Instant::now());
                        // The run gate, asked after the backoff so a
                        // closing or held-back gate is read as it is now.
                        // Held through this leg's settle below.
                        let permit = match workspaces.begin_start(&workspace_id) {
                            Ok(permit) => permit,
                            Err(refusal) => {
                                tracing::info!(workspace_id = %workspace_id, %refusal, "capsule supervisor watchdog: restart refused by the run gate -- not restarting");
                                return;
                            }
                        };
                        // ADR 0043 decision 29: a process spawn never runs
                        // on a Tokio worker. Clones are the closure's OWN
                        // copies (`'static` + `Send`, required across the
                        // `.await` below) -- the loop's own locals are
                        // untouched and reused on the NEXT iteration.
                        let exe = sot_capsule_exe.clone();
                        let dir = state_dir.clone();
                        let argv_for_spawn = argv.clone();
                        let cwd_for_spawn = cwd.clone();
                        let agent_name_for_spawn = agent_name.clone();
                        let workspace_id_for_spawn = workspace_id.clone();
                        let slug_for_spawn = slug.clone();
                        // Accounts brief + ADR 0046 decision 6: read at
                        // RESTART time, never captured at install time.
                        // `workspace.reauth` moves a live row's account
                        // while its watchdog is parked on the OLD leg, so
                        // a captured pair would respawn on the login the
                        // row no longer has -- a live leg under a record
                        // that says otherwise. Same resolve, same
                        // `unwrap_or_default` degradation, as the first
                        // leg's in `spawn_and_watch`.
                        let (agent_kind_for_spawn, account_for_spawn) = workspaces
                            .resolve(Some(&workspace_id))
                            .map(|ws| (ws.agent(), ws.account()))
                            .unwrap_or_default();
                        let spawn_result = tokio::task::spawn_blocking(move || {
                            spawn_detached_supervisor(
                                &permit,
                                &exe,
                                &dir,
                                StartMode::Resume,
                                &argv_for_spawn,
                                &cwd_for_spawn,
                                &agent_name_for_spawn,
                                &workspace_id_for_spawn,
                                &slug_for_spawn,
                                &agent_kind_for_spawn,
                                &account_for_spawn,
                            )
                            .map(|child| (child, permit))
                        })
                        .await;
                        match spawn_result {
                            Ok(Ok((child, permit))) => {
                                // Settle BEFORE this guard drops — the
                                // SAME shared wait `spawn_and_watch`
                                // itself uses; see `settle_after_spawn`'s
                                // own doc for why a fresh spawn cannot
                                // skip this without reopening the exact
                                // guard-release race this restart's own
                                // recheck above just closed. A fresh leg
                                // begins a fresh epoch exactly as the
                                // first spawn does: by being adopted
                                // from what it answers.
                                let settle_dir = state_dir.clone();
                                let settle_wsid = workspace_id.clone();
                                let settled = tokio::task::spawn_blocking(move || settle_after_spawn(&settle_dir, &settle_wsid)).await;
                                // Always OVERWRITE, never merely set: a
                                // settle that yields no `Phase` clears
                                // the PREVIOUS leg's identity, or the
                                // next terminal mark would be credited
                                // to a spawn that is already dead.
                                current_identity = match &settled {
                                    Ok((_phase, observation)) => identity_of(observation),
                                    Err(_join_err) => None,
                                };
                                if let (Ok((_phase, observation)), Some(ws)) = (settled, workspaces.resolve(Some(&workspace_id))) {
                                    observe_with_adoption(&ws, observation);
                                }
                                drop(permit);
                                leg_opt = Some(child);
                            }
                            Ok(Err(e)) if e.kind() == ErrorKind::Unsupported => {
                                // `qualified_state_root` refused (ADR 0043
                                // decision 23: the state root went unqualified
                                // out from under a live row -- an `XDG_STATE_HOME`
                                // change, a remounted volume). No retry can change
                                // that without operator action -- mark terminal
                                // now (the error names the recovery).
                                tracing::error!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: unqualified state root -- marking terminal, no restart");
                                observe_terminal(&workspaces, &workspace_id, current_identity);
                                return;
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: restart spawn failed");
                            }
                            Err(join_err) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %join_err, "capsule supervisor watchdog: restart spawn task panicked");
                            }
                        }
                        // `_held` drops here -- released only once the
                        // new leg exists, or the attempt has failed.
                    }
                }
            }
        });
    }

    /// On daemon startup: resume every REGISTERED capsule workspace's
    /// supervisor whose voyage pointer has ALREADY been published (rule
    /// B, shrink round). A row with NO published pointer — never started
    /// at all, OR a leg that crashed before ever publishing one (the
    /// exact pre-pointer crash window ADR 0041 names) — is SKIPPED
    /// entirely: `pty.open`'s start-on-attach (`ensure_started`) is what
    /// starts those now, not this scan. Before rule B this scan launched
    /// `--resume` unconditionally for EVERY registered row, including
    /// ones with no pointer at all — `sot-capsule supervise --resume`
    /// against a workspace with no leg to adopt and no pointer to found
    /// one against simply fails (exit 69), leaving a bare state directory
    /// behind and reading back as a misleading row on the owner's own box
    /// (the field finding behind this shrink round).
    ///
    /// ADR 0043 decision 33 (Codex review, 2026-09-11): every candidate's
    /// guard is taken FIRST, via `try_lock` — never blocking, and a row
    /// already busy (an attach's own `ensure_started`/`resume_if_absent`
    /// got there first) is simply skipped, that caller's own attempt
    /// being the one that counts — and the decision itself is then made
    /// UNDER that guard by [`resume_locked`], the SAME function every
    /// other resume path shares: a row still alive (from a previous
    /// daemon lifetime, spawned detached by ADR 0042 design, outliving
    /// the daemon that just restarted) reports its own current phase and
    /// spawns nothing — no second `--resume` leg races the live one's
    /// `supervisor.lock`, and no watchdog is installed for a process this
    /// daemon did not itself launch (decision 33: "a watchdog exists only
    /// for a `Child` the daemon launched"). If that authority later goes
    /// quiet, the next attach's own `resume_if_absent`/`ensure_started`
    /// spawns and watches a FRESH leg under the SAME row's guard —
    /// reactive, same as every other resume path (a list never resumes).
    /// A genuinely unreachable row spawns `--resume`, settling before
    /// this task returns (`resume_locked` -> `start_supervisor` ->
    /// `settle_after_spawn`).
    ///
    /// A state directory with NO matching registry entry is left
    /// COMPLETELY untouched, logged once (ADR 0042: "the daemon's
    /// workspace list is the list" — an orphan is not addressable
    /// through any op, so resuming it would create a live, unaddressable
    /// process; deleted, the bare-shell fallback an earlier version used
    /// here).
    ///
    /// Runs off the startup critical path (finding 10): `server.rs`
    /// calls this via `tokio::spawn`, never awaited, and every probe/spawn
    /// inside it is bounded to `LANE_CONCURRENCY` concurrent attempts via
    /// a semaphore — thousands of preserved workspaces cannot turn this
    /// into an unbounded synchronous fan-out before the listener binds.
    pub async fn resume_all(state_root: PathBuf, workspaces: Workspaces) {
        // 2026-09-04 amendment: the inert default anchor is never resumed
        // here either, even on the rare box where a pointer already exists
        // for it (a hand-edited toml that dropped its agent after a prior
        // real run). Every OTHER `agent == "none"` capsule row still
        // resumes: `agent_argv("none", None)` is a real leg (the bare platform
        // shell). The predicate — and why its runtime term matters — is
        // `Workspaces::is_inert_default_anchor`.
        let capsule_rows: Vec<Arc<crate::workspaces::Workspace>> = workspaces
            .list()
            .into_iter()
            .filter(|ws| ws.runtime == "capsule")
            .filter(|ws| !workspaces.is_inert_default_anchor(ws))
            .collect();

        // Covers both boot load and adoption of a still-live prior authority.
        for ws in &capsule_rows {
            super::observer::ensure_running(&workspaces, ws);
        }

        let candidates: Vec<(String, String, PathBuf, String, String)> = capsule_rows
            .into_iter()
            .filter(|ws| {
                let state_dir = super::state_dir_for(&state_root, &ws.workspace_id);
                sot_log::pointer::pointer_path(&state_dir).is_file()
            })
            .map(|ws| {
                (
                    ws.workspace_id.clone(),
                    ws.agent(),
                    ws.project_root.clone(),
                    ws.agent_name(),
                    ws.slug.clone(),
                )
            })
            .collect();

        let semaphore = Arc::new(tokio::sync::Semaphore::new(LANE_CONCURRENCY));
        let mut joins = Vec::with_capacity(candidates.len());
        for (workspace_id, agent_kind, project_root, agent_name, slug) in candidates {
            let permit = semaphore.clone();
            let state_root = state_root.clone();
            let workspaces = workspaces.clone();
            joins.push(tokio::spawn(async move {
                let _permit = permit.acquire_owned().await;
                let Some(guard) = workspaces.capsule_guard(&workspace_id) else {
                    return;
                };
                let lock_result = guard.try_lock();
                match lock_result {
                    Ok(_held) => {
                        let workspace_id_for_log = workspace_id.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            resume_locked(&state_root, &workspace_id, &agent_kind, &agent_name, &slug, &project_root, workspaces)
                        })
                        .await;
                        match result {
                            Ok(Ok(phase)) => {
                                tracing::info!(workspace_id = %workspace_id_for_log, phase, "capsule workspace resume-scan: row resolved");
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(workspace_id = %workspace_id_for_log, error = %e, "capsule workspace resume-scan: row resolution failed");
                            }
                            Err(join_err) => {
                                tracing::warn!(workspace_id = %workspace_id_for_log, error = %join_err, "capsule workspace resume-scan: resume task panicked");
                            }
                        }
                    }
                    Err(_) => {
                        tracing::debug!(workspace_id = %workspace_id, "capsule workspace resume-scan: row's guard busy; skipping");
                    }
                }
            }));
        }
        for j in joins {
            let _ = j.await;
        }

        log_registryless_state_dirs(&state_root, &workspaces);
        log_orphaned_state_dirs(&state_root, &workspaces);
    }

    /// One log line naming every `<state-root>/workspaces/*` directory
    /// with no matching registry entry — diagnostic only, never acted on
    /// (see [`resume_all`]'s own doc).
    fn log_registryless_state_dirs(state_root: &Path, workspaces: &Workspaces) {
        let dir = state_root.join("workspaces");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return,
            Err(e) => {
                tracing::debug!(dir = ?dir, error = %e, "capsule workspace resume-scan: could not read the state root for the registryless-directory log sweep");
                return;
            }
        };
        let known: std::collections::HashSet<String> =
            workspaces.list().into_iter().map(|ws| ws.workspace_id.clone()).collect();
        let orphans: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|id| !known.contains(id))
            .collect();
        if !orphans.is_empty() {
            tracing::warn!(
                count = orphans.len(), ids = ?orphans,
                "capsule workspace resume-scan: state directories with no matching registry entry -- \
                 left untouched (ADR 0042: the workspace list is the list)"
            );
        }
    }

    /// The reverse of [`log_registryless_state_dirs`]: a REGISTERED
    /// capsule row whose `state_dir` does not exist under THIS daemon's
    /// own state root — a DIAGNOSTIC candidate list only, never a proof.
    /// A row that went through `workspace.create` and reached the
    /// registry always had a `start_supervisor` call succeed there (a
    /// failed spawn rolls the row and its toml back before either is
    /// persisted, `handlers.rs`'s create rollback), and that success is
    /// exactly what creates the directory — but a row can ALSO reach the
    /// registry by a pre-seeded or hand-authored toml that has never been
    /// through `workspace.create` at all (a legitimate, tested shape:
    /// "Rule H" in `capsule_workspaces.rs`'s own integration suite), and
    /// reads identically here — no state dir, no pointer, "never started
    /// yet" — RIGHT UP UNTIL its first attach spawns it for real. This
    /// function cannot and does not try to tell the two apart (see
    /// `workspace.list`'s own phase, which deliberately does not either —
    /// decision 33's Rule B: "neither has ever had a real run"); it only
    /// NAMES every such row, once, at boot — never from the per-row
    /// lifecycle observer's own `POLL_INTERVAL` loop, which would
    /// otherwise repeat the same line forever — so an operator staring at
    /// `sotd.log` after the field defect this closes (a leaked scratch-
    /// daemon registry row) has a lead to start from. Removal is still
    /// only ever `workspace.destroy`'s to decide, via `end_run`'s own
    /// PROOF (a real lane connect, decision 27's absent shape) — which
    /// needs no such distinction either: nothing running is nothing
    /// running, whether the row is freshly seeded or truly abandoned.
    /// STAT STRICTLY, matching `end_run`: only `ErrorKind::NotFound` is
    /// "missing" — a permission or I/O error says nothing about whether
    /// the directory is actually gone, so it is skipped (not logged)
    /// rather than guessed at.
    fn log_orphaned_state_dirs(state_root: &Path, workspaces: &Workspaces) {
        for ws in workspaces.list() {
            if ws.runtime != "capsule" {
                continue;
            }
            let state_dir = super::state_dir_for(state_root, &ws.workspace_id);
            match std::fs::metadata(&state_dir) {
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                _ => continue,
            }
            tracing::warn!(
                workspace_id = %ws.workspace_id, slug = %ws.slug, state_dir = ?state_dir,
                "capsule workspace resume-scan: registered row has no state directory under this \
                 daemon's state root -- reads as an ordinary stopped row (indistinguishable here \
                 from one simply never started yet); workspace.destroy removes it once no \
                 supervisor answers its lane"
            );
        }
    }
}

pub use runtime::*;

/// One lifecycle observer per capsule row -- the SINGLE writer of `Workspace::phase`.
pub mod observer {
    use super::{local_phase, phase_for_missing_pointer, state_dir_for};
    use crate::workspaces::{Observation, SupervisorIdentity, Workspace, Workspaces};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    /// Matches the attach client's own liveness poll interval.
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    /// The ONE call site that feeds an observation into `ws`'s phase cell; a rejection logs at debug, never an error.
    pub(crate) fn observe(ws: &Workspace, observation: Observation) {
        if !ws.apply_phase_observation(observation) {
            tracing::debug!(workspace_id = %ws.workspace_id, "capsule workspace observer: observation rejected (stale, or a latched terminal phase)");
        }
    }

    /// Idempotent: ensures a lifecycle-observer task runs for `ws`, never from `Workspaces::insert`.
    pub fn ensure_running(workspaces: &Workspaces, ws: &Arc<Workspace>) {
        if workspaces.has_observer(&ws.workspace_id) {
            return;
        }
        let Some(state_root) = sot_log::state_dir::sot_state_dir() else {
            return;
        };
        let state_dir = state_dir_for(&state_root, &ws.workspace_id);
        let persistent = sot_log::supervisor_client::Persistent::new(&state_dir);
        // Obtained outside the loop so removal can interrupt a round blocked in `spawn_blocking`.
        let cancel_handle = persistent.cancel_handle();
        let cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || cancel_handle.cancel());
        let ws_for_task = ws.clone();
        let workspaces_for_task = workspaces.clone();
        let workspace_id = ws.workspace_id.clone();
        let handle = tokio::spawn(run(ws_for_task, state_dir, persistent, workspaces_for_task));
        workspaces.install_observer(&workspace_id, handle, cancel);
    }

    /// Immediate first round, then every `POLL_INTERVAL`; exits once its row is gone.
    async fn run(
        ws: Arc<Workspace>,
        state_dir: PathBuf,
        mut persistent: sot_log::supervisor_client::Persistent,
        workspaces: Workspaces,
    ) {
        loop {
            if workspaces.resolve(Some(&ws.workspace_id)).is_none() {
                return;
            }
            let dir = state_dir.clone();
            let (returned, observation) = tokio::task::spawn_blocking(move || {
                let obs = poll_once(&dir, &mut persistent);
                (persistent, obs)
            })
            .await
            .unwrap_or_else(|_join_err| {
                (sot_log::supervisor_client::Persistent::new(&state_dir), Observation::Failed)
            });
            persistent = returned;
            observe(&ws, observation);
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// One BLOCKING round: no pointer -> `Stopped`; otherwise one `status` call via [`local_phase`].
    fn poll_once(state_dir: &Path, persistent: &mut sot_log::supervisor_client::Persistent) -> Observation {
        if phase_for_missing_pointer(sot_log::pointer::pointer_path(state_dir).is_file()).is_some() {
            return Observation::Stopped;
        }
        match persistent.status() {
            Ok(report) => Observation::Phase {
                phase: local_phase(report.phase),
                supervisor: SupervisorIdentity { pid: report.pid, created: report.created },
                voyage: report.voyage.as_deref().and_then(|v| v.parse().ok()),
            },
            Err(sot_log::Error::VersionSkew) => Observation::Foreign,
            Err(_) => Observation::Failed,
        }
    }
}

#[cfg(test)]
mod observer_tests {
    use super::observer::observe;
    use crate::workspaces::{Observation, Phase, SupervisorIdentity, Workspace, Workspaces};
    use std::path::PathBuf;

    /// A registered capsule row with its epoch begun at `supervisor` (RA).
    fn seeded_capsule_row(supervisor: SupervisorIdentity) -> std::sync::Arc<Workspace> {
        let ws = unclaimed_capsule_row();
        ws.begin_supervisor_epoch(supervisor);
        ws
    }

    /// The same row with NO epoch yet -- the empty cell the ruling's
    /// addition (a) is about: a row the daemon has spawned for but whose
    /// lane has not yet answered, or one only ever read as stopped.
    fn unclaimed_capsule_row() -> std::sync::Arc<Workspace> {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            "observer-test",
            PathBuf::from("/tmp/sot-observer-test"),
            false,
            "none".to_string(),
            String::new(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        reg.insert(ws)
    }

    fn identity(pid: u32, created: u64) -> SupervisorIdentity {
        SupervisorIdentity { pid, created }
    }

    fn phase_obs(phase: Phase, supervisor: SupervisorIdentity) -> Observation {
        Observation::Phase { phase, supervisor, voyage: None }
    }

    #[test]
    fn begin_supervisor_epoch_resets_phase_voyage_and_failures() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        assert_eq!(ws.phase(), Phase::Stopped, "a fresh epoch starts Stopped");
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready);

        ws.begin_supervisor_epoch(identity(2, 200));
        assert_eq!(ws.phase(), Phase::Stopped, "a new epoch resets phase");
    }

    /// RA blocker 2: judged by identity EQUALITY, never by timestamp -- a same-tick stranger is never "newer."
    #[test]
    fn a_different_supervisor_is_rejected_even_with_an_equal_or_newer_timestamp() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready);

        observe(&ws, phase_obs(Phase::Ending, identity(2, 100)));
        assert_eq!(ws.phase(), Phase::Ready, "a same-tick stranger must never be accepted");

        observe(&ws, phase_obs(Phase::Ending, identity(3, 500)));
        assert_eq!(ws.phase(), Phase::Ready, "a newer-timestamped stranger must never be accepted");
    }

    #[test]
    fn unreachable_needs_two_consecutive_failed_rounds() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready);

        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Ready, "a single failed round must not move the phase");

        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Unreachable);

        observe(&ws, phase_obs(Phase::Ready, a));
        observe(&ws, Observation::Failed);
        observe(&ws, phase_obs(Phase::Ready, a));
        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Ready, "a success between two failures resets the count");
    }

    /// RA blocker 1: `Terminal` is supervisor-scoped, not voyage-scoped -- applies regardless of phase/voyage.
    #[test]
    fn a_terminal_observation_with_no_voyage_applies_even_over_ended_no_respawn() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        let v1 = uuid::Uuid::from_u128(1);
        observe(&ws, Observation::Phase { phase: Phase::EndedNoRespawn, supervisor: a, voyage: Some(v1) });
        assert_eq!(ws.phase(), Phase::EndedNoRespawn);

        observe(&ws, phase_obs(Phase::Terminal, a));
        assert_eq!(ws.phase(), Phase::Terminal, "Terminal must not be hidden behind an EndedNoRespawn latch");
    }

    #[test]
    fn a_terminal_observation_latches_and_only_a_fresh_epoch_clears_it() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));
        observe(&ws, phase_obs(Phase::Terminal, a));
        assert_eq!(ws.phase(), Phase::Terminal);

        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Terminal, "terminal latches within its own epoch");
        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Terminal, "terminal latches across a failed round too");

        // Only a fresh epoch (a new spawn or adoption) clears it.
        let b = identity(2, 200);
        ws.begin_supervisor_epoch(b);
        observe(&ws, phase_obs(Phase::Ready, b));
        assert_eq!(ws.phase(), Phase::Ready);
    }

    /// `EndedNoRespawn` latches for its VOYAGE within the epoch; a strictly newer voyage clears it.
    #[test]
    fn ended_no_respawn_latches_for_its_voyage() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        let v1 = uuid::Uuid::from_u128(1);
        let v2 = uuid::Uuid::from_u128(2);

        observe(&ws, Observation::Phase { phase: Phase::EndedNoRespawn, supervisor: a, voyage: Some(v1) });
        assert_eq!(ws.phase(), Phase::EndedNoRespawn);

        observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: a, voyage: Some(v1) });
        assert_eq!(ws.phase(), Phase::EndedNoRespawn, "the same voyage reported again must never clear the latch");

        observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: a, voyage: Some(v2) });
        assert_eq!(ws.phase(), Phase::Ready, "a strictly newer voyage supersedes the latch");
    }

    /// Addition (a): an empty cell takes the FIRST prover -- and only
    /// the first. The rejection half is the same rule RA blocker 2
    /// asserts, re-run against the adopting branch to prove the adoption
    /// is scoped to `None` and never widens who may claim a claimed row.
    #[test]
    fn an_empty_cell_adopts_its_first_prover_and_then_judges_strangers() {
        let a = identity(1, 100);
        let ws = unclaimed_capsule_row();
        assert_eq!(ws.current_supervisor(), None, "an unclaimed row has no epoch");

        // Two failed rounds first: an empty cell that is already
        // `Unreachable` must still adopt, and adoption must reset the
        // failure count, exactly as `begin_supervisor_epoch` does.
        observe(&ws, Observation::Failed);
        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Unreachable);

        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.current_supervisor(), Some(a), "the first prover becomes the epoch");
        assert_eq!(ws.phase(), Phase::Ready);

        observe(&ws, phase_obs(Phase::Ending, identity(2, 100)));
        assert_eq!(ws.phase(), Phase::Ready, "a claimed cell still rejects a stranger");
        assert_eq!(ws.current_supervisor(), Some(a));
    }

    /// Addition (b): a bootstrap-failing leg latches its row `terminal`
    /// without an identity, and a later genuine authority adopts and
    /// clears it -- a row whose lane answers is not terminal.
    #[test]
    fn terminal_unclaimed_latches_an_empty_cell_and_a_later_authority_clears_it() {
        let ws = unclaimed_capsule_row();
        assert!(ws.apply_phase_observation(Observation::TerminalUnclaimed));
        assert_eq!(ws.phase(), Phase::Terminal);
        assert_eq!(ws.current_supervisor(), None, "the mark leaves the cell claimable");

        observe(&ws, Observation::Stopped);
        assert_eq!(ws.phase(), Phase::Terminal, "the latch holds against a plain stopped read");

        let a = identity(7, 700);
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready, "an authority that actually answers clears the latch");
        assert_eq!(ws.current_supervisor(), Some(a));
    }

    /// The other half of (b): a cell claimed at any point in this
    /// daemon's life is closed to it, so a stale watchdog can never
    /// latch a row it no longer owns.
    #[test]
    fn terminal_unclaimed_is_refused_by_a_claimed_cell() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));

        assert!(!ws.apply_phase_observation(Observation::TerminalUnclaimed));
        assert_eq!(ws.phase(), Phase::Ready);
    }

    /// `activation_error` is retained until the next attempt; orthogonal to phase.
    #[test]
    fn activation_error_is_independent_of_phase_and_clears_on_the_next_attempt() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        assert_eq!(ws.activation_error(), None);

        ws.set_activation_error(Some("capsule spawn failed: boom".to_string()));
        assert_eq!(ws.phase(), Phase::Stopped, "an activation error must never move phase on its own");

        observe(&ws, phase_obs(Phase::Terminal, a));
        assert_eq!(ws.phase(), Phase::Terminal);
        assert_eq!(
            ws.activation_error(),
            Some("capsule spawn failed: boom".to_string()),
            "a phase observation must never clear a pending activation_error -- only the next attempt does"
        );

        ws.set_activation_error(None);
        assert_eq!(ws.activation_error(), None);
    }
}

/// ADR 0042 amendment (2026-09-07), "a session types into and reads a
/// sibling row": the daemon's own HEADLESS client on a capsule lane — the
/// same [`sot_log::fe_client_io::FeAttachClient`] the frontend's drawer
/// uses, run on the daemon side with no viewport and no user watching.
/// `type_into` takes the pen only long enough to deliver ONE `input` frame
/// and never resizes the pane (ADR 0041's take-on-first-input semantics,
/// applied to a second kind of client); `screen_of` attaches as a pure
/// WATCHER and never takes at all. Platform-neutral: gated the same as
/// `mod runtime` above, so this module simply does not exist on a host
/// that cannot run a capsule row in the first place. (macOS lane,
/// corrected: `FeAttachClient` is NO LONGER what gates this —
/// `sot_log::fe_client_io` is ungated since ADR 0045 decision 1, and only
/// its `PlatformEndpoint`-typed default is cfg'd. The reason was solely
/// `mod runtime`'s own gate, and that gate is gone — so is this one.)
pub mod headless {
    use std::path::Path;
    use sot_protocol::PtyEnter;
    use std::time::{Duration, Instant};

    use sot_log::client::PlatformEndpoint;
    use sot_log::fe_client::TAKE_QUEUE_CAP;
    use sot_log::fe_client_io::{FeAttachClient, InputOutcome};
    use sot_log::state_dir::state_dir_hash;

    /// This daemon build's own concrete attach client — always the real
    /// platform endpoint (pipes on Windows, a Unix socket on Linux). No
    /// caller of this module ever needs to name `E` itself.
    pub(crate) type Client = FeAttachClient<PlatformEndpoint>;

    /// How often the daemon polls its own headless client. Independent of
    /// (and much finer than) the deadline a caller passes in — this is
    /// just the local spin-wait granularity, not a protocol budget.
    pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(20);

    /// Bound for the client's own worker-thread join on every exit path
    /// (`FeAttachClient::shutdown`). Generous relative to the worker's
    /// 100 ms tick, but still a REAL bound — "on EVERY exit path: drop the
    /// client, then observe the worker's closure" (ADR 0042 amendment §3).
    pub(crate) const SHUTDOWN_WAIT: Duration = Duration::from_millis(500);

    /// What a headless op failed to do, and where. `phase` is one of
    /// `size` (the size gate, before any attach), `attach`, `checkpoint`,
    /// `take` (the pen was asked for but never granted, and — since
    /// nothing reached [`Client::send_input`]'s own wire flush yet —
    /// `submitted` is `false` here), `input` (a definite
    /// `input_refused_stale`; never retried by this module), `record`
    /// (the record's own verdict is UNKNOWABLE: either the wire said
    /// `input_delivery_unknown`, or the deadline expired after the input
    /// had already been handed to the lane — both mean the same thing to
    /// a caller: do not retry, and do not assume failure either), or
    /// `detach` (reserved for the shutdown bound itself; see its own doc —
    /// today this is only ever logged, never returned as an `Err`).
    #[derive(Debug)]
    pub struct HeadlessError {
        pub phase: &'static str,
        pub detail: String,
        /// `true` iff the input had already been handed to the lane
        /// (`Client::send_input` called) when the failure/expiry hit —
        /// the daemon's own signal to answer `capsule_input_unknown`
        /// rather than a flat failure (ADR 0042 amendment §2).
        pub submitted: bool,
    }

    /// `screen_of`'s own result: the row's current, visible screen.
    /// `cursor` is `(row, col)`, matching `vt100_ctt::Screen::
    /// cursor_position`'s own order.
    pub struct ScreenShot {
        pub cols: u16,
        pub rows: u16,
        pub lines: Vec<String>,
        pub cursor: Option<(u16, u16)>,
    }

    /// Types `bytes` into the row at `state_dir` as `controller_id`,
    /// taking the pen only long enough to deliver them — never resizing
    /// the pane (a headless client has no viewport to size it to). One
    /// absolute `deadline` covers attach, checkpoint, take, and the wait
    /// for the wire's own verdict on the input; every exit path drops the
    /// client and observes the worker's closure within [`SHUTDOWN_WAIT`].
    /// Returns the number of payload bytes delivered (never counting a
    /// trailing Enter byte the caller may have already folded in — this
    /// function has no opinion on that, it delivers exactly what it is
    /// given).
    pub fn type_into(
        state_dir: &Path,
        controller_id: &str,
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<usize, HeadlessError> {
        if bytes.len() > TAKE_QUEUE_CAP {
            return Err(HeadlessError {
                phase: "size",
                detail: format!(
                    "payload is {} bytes, exceeding the take queue cap of {TAKE_QUEUE_CAP} bytes",
                    bytes.len()
                ),
                submitted: false,
            });
        }
        if bytes.is_empty() {
            // "an empty payload succeeds without taking" (ADR 0042
            // amendment review) — nothing to attach for.
            return Ok(0);
        }

        let mut client = attach(state_dir, controller_id)?;
        if let Err(e) = wait_for_checkpoint(&mut client, deadline) {
            client.shutdown(SHUTDOWN_WAIT);
            return Err(e);
        }
        let result = send_and_wait_recorded(&mut client, bytes, deadline);
        client.shutdown(SHUTDOWN_WAIT);
        result
    }

    /// The send/wait-for-verdict loop, factored out of [`type_into`] so
    /// [`write_and_enter`] can call it twice (text, then Enter) on ONE
    /// continuous attach — a mid-sequence pen loss then surfaces as an
    /// ordinary `RefusedStale`/`is_dead()` on the same client.
    fn send_and_wait_recorded(client: &mut Client, bytes: &[u8], deadline: Instant) -> Result<usize, HeadlessError> {
        let expected = bytes.len() as u64;
        let before = client.recorded_bytes();
        client.send_input(bytes);
        loop {
            client.pump();
            if let Some(outcome) = client.last_input_outcome() {
                match outcome {
                    InputOutcome::Recorded => {
                        if client.recorded_bytes().saturating_sub(before) >= expected {
                            return Ok(bytes.len());
                        }
                        // A single `send_input` call is always flushed as
                        // ONE input frame in practice (the payload already
                        // fits under `TAKE_QUEUE_CAP`, so nothing splits
                        // it) — this branch should be unreachable, but
                        // correctness does not depend on that: keep
                        // polling for the rest, bounded by the same
                        // deadline, rather than declaring victory early.
                        if Instant::now() >= deadline {
                            return Err(HeadlessError {
                                phase: "record",
                                detail: "deadline exceeded before the whole payload was recorded"
                                    .to_string(),
                                submitted: true,
                            });
                        }
                        std::thread::sleep(POLL_INTERVAL);
                    }
                    InputOutcome::RefusedStale => {
                        return Err(HeadlessError {
                            phase: "input",
                            detail: "input refused as stale (the take epoch changed); \
                                     this op is never retried"
                                .to_string(),
                            submitted: true,
                        });
                    }
                    InputOutcome::DeliveryUnknown => {
                        return Err(HeadlessError {
                            phase: "record",
                            detail: "input delivery unknown".to_string(),
                            submitted: true,
                        });
                    }
                }
                continue;
            }
            if client.is_dead() {
                return Err(HeadlessError {
                    phase: "take",
                    detail: client.status_line().to_string(),
                    submitted: true,
                });
            }
            if Instant::now() >= deadline {
                return Err(HeadlessError {
                    phase: "record",
                    detail: "deadline exceeded waiting for the input to be recorded".to_string(),
                    submitted: true,
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// A capsule row's SPLIT `enter: true` write: text, a bounded PACING
    /// wait, then Enter as its own write — makes NO submission claim (a
    /// screen snapshot cannot prove one) and never retries either write.
    ///
    /// 1. [`type_into`]'s own size gate runs first, before any attach.
    /// 2. Writes the text — a hard error, never retried, on any failure.
    /// 3. PACES only (claims nothing): waits for the screen to hold still
    ///    `quiet_budget`, bounded overall at `pacing_budget`.
    /// 4. Writes Enter under the text's own grant (headless never
    ///    re-takes): a pen change in between is refused stale by the
    ///    supervisor, so `enter: not_sent`. Once the text is recorded
    ///    (step 2), this ALWAYS answers `Ok`: an Enter failure is never a
    ///    hard `Err` (a retried caller could double the text); it is
    ///    `not_sent` for the stale refusal and `unknown` for every other
    ///    failure (see [`enter_outcome`]).
    /// 5. Never retries: the worker's auto-retake after a stale refusal
    ///    is disabled for headless transactions.
    ///
    /// RESIDUAL RISK: this never checks whether a human already has an
    /// unsent draft here (parity with the old tmux path, which never did
    /// either) — a real check needs attach-proto v3's pen-holder signal
    /// (`PenSnapshot`/`holder`), left to the Stage 2 resident-attach lanes.
    ///
    /// Returns `(bytes_written, enter)`: `enter` is `sent` only when the
    /// Enter byte was written and recorded, never a claim that codex
    /// treated it as a submitted turn; `not_sent` when the supervisor
    /// refused it; `unknown` otherwise.
    pub fn write_and_enter(
        state_dir: &Path,
        controller_id: &str,
        text: &[u8],
        op_budget: Duration,
        quiet_budget: Duration,
        pacing_budget: Duration,
    ) -> Result<(usize, PtyEnter), HeadlessError> {
        if text.len() > TAKE_QUEUE_CAP {
            return Err(HeadlessError {
                phase: "size",
                detail: format!(
                    "payload is {} bytes, exceeding the take queue cap of {TAKE_QUEUE_CAP} bytes",
                    text.len()
                ),
                submitted: false,
            });
        }

        let mut client = attach(state_dir, controller_id)?;
        if let Err(e) = wait_for_checkpoint(&mut client, Instant::now() + op_budget) {
            client.shutdown(SHUTDOWN_WAIT);
            return Err(e);
        }
        let out = type_and_pace(&mut client, text, op_budget, quiet_budget, pacing_budget)
            .map(|n| (n, enter_outcome(send_enter(&mut client, op_budget))));
        client.shutdown(SHUTDOWN_WAIT);
        out
    }

    /// What the Enter write's result says to a `pty.input` caller: only the stale refusal (phase `"input"`, the
    /// one outcome where nothing was written, see `send_and_wait_recorded`) is `NotSent`; every other failure may
    /// have reached the agent.
    pub(crate) fn enter_outcome(r: Result<(), HeadlessError>) -> PtyEnter {
        match r {
            Ok(()) => PtyEnter::Sent,
            Err(e) if e.phase == "input" => PtyEnter::NotSent,
            Err(_) => PtyEnter::Unknown,
        }
    }

    /// [`write_and_enter`]'s steps 2-3 (type, then wait for the screen to settle) over an already attached,
    /// checkpointed client; the caller shuts the client down.
    pub(crate) fn type_and_pace(
        client: &mut Client,
        text: &[u8],
        op_budget: Duration,
        quiet_budget: Duration,
        pacing_budget: Duration,
    ) -> Result<usize, HeadlessError> {
        let n = if text.is_empty() {
            0
        } else {
            match send_and_wait_recorded(client, text, Instant::now() + op_budget) {
                Ok(n) => n,
                Err(e) => return Err(e),
            }
        };
        // `SOT_TEST_PACING_HOLD` (test-only, the `SOT_TEST_ACTIVATION_BARRIER`
        // convention): hold pacing to its full bound. Terminal output batches,
        // so a scripted test load cannot keep the screen changing every poll.
        let pacing_hold = std::env::var_os("SOT_TEST_PACING_HOLD").is_some();

        let pacing_deadline = Instant::now() + pacing_budget;
        let mut previous = current_lines(client);
        let mut last_change_at = Instant::now();
        loop {
            let quiet_elapsed = !pacing_hold && Instant::now().duration_since(last_change_at) >= quiet_budget;
            if quiet_elapsed || Instant::now() >= pacing_deadline {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
            client.pump();
            let lines = current_lines(client);
            if lines != previous {
                previous = lines;
                last_change_at = Instant::now();
            }
        }

        Ok(n)
    }

    /// [`write_and_enter`]'s step 4: the Enter byte, written and recorded. Doc above.
    pub(crate) fn send_enter(client: &mut Client, op_budget: Duration) -> Result<(), HeadlessError> {
        send_and_wait_recorded(client, &[0x0d], Instant::now() + op_budget).map(|_| ())
    }

    /// Current screen lines, top to bottom, trailing spaces trimmed —
    /// [`screen_of`]'s own shape, off an already-pumped client.
    fn current_lines(client: &Client) -> Vec<String> {
        let (_, cols) = client.screen().size();
        client.screen().rows(0, cols).map(|line| line.trim_end().to_string()).collect()
    }

    /// Reads the current, visible screen of the row at `state_dir` as a
    /// pure WATCHER — never takes the pen, never sends input. Same
    /// deadline/shutdown discipline as [`type_into`].
    pub fn screen_of(
        state_dir: &Path,
        controller_id: &str,
        deadline: Instant,
    ) -> Result<ScreenShot, HeadlessError> {
        let mut client = attach(state_dir, controller_id)?;
        if let Err(e) = wait_for_checkpoint(&mut client, deadline) {
            client.shutdown(SHUTDOWN_WAIT);
            return Err(e);
        }
        let (rows, cols) = client.screen().size();
        let lines: Vec<String> =
            client.screen().rows(0, cols).map(|line| line.trim_end().to_string()).collect();
        let cursor = Some(client.screen().cursor_position());
        client.shutdown(SHUTDOWN_WAIT);
        Ok(ScreenShot { cols, rows, lines, cursor })
    }

    pub(crate) fn attach(state_dir: &Path, controller_id: &str) -> Result<Client, HeadlessError> {
        Client::attach_headless(PlatformEndpoint::default(), state_dir_hash(state_dir), controller_id.to_string()).map_err(|e| {
            HeadlessError { phase: "attach", detail: e.to_string(), submitted: false }
        })
    }

    pub(crate) fn wait_for_checkpoint(client: &mut Client, deadline: Instant) -> Result<(), HeadlessError> {
        loop {
            client.pump();
            if client.is_checkpointed() {
                return Ok(());
            }
            if client.is_dead() {
                return Err(HeadlessError {
                    phase: "checkpoint",
                    detail: client.status_line().to_string(),
                    submitted: false,
                });
            }
            if Instant::now() >= deadline {
                return Err(HeadlessError {
                    phase: "checkpoint",
                    detail: "deadline exceeded before a checkpoint arrived".to_string(),
                    submitted: false,
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(test)]
mod headless_size_gate_tests {
    // Pure size-gate tests: `type_into` checks the payload length BEFORE
    // ever attempting an attach, so these need no supervisor, no state
    // dir on disk, and no real process at all — a nonexistent path is
    // fine, and a real attach attempt against it would prove the test
    // wrong (the size gate must short-circuit before that).
    use super::headless::{enter_outcome, type_into, write_and_enter, HeadlessError};
    use sot_protocol::PtyEnter;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn enter_outcome_tells_unknown_from_not_sent() {
        let stale = HeadlessError { phase: "input", detail: "stale".into(), submitted: true };
        let unknown = HeadlessError { phase: "record", detail: "unknown".into(), submitted: true };
        let take = HeadlessError { phase: "take", detail: "dead".into(), submitted: true };
        assert_eq!(enter_outcome(Ok(())), PtyEnter::Sent);
        assert_eq!(enter_outcome(Err(stale)), PtyEnter::NotSent);
        assert_eq!(enter_outcome(Err(unknown)), PtyEnter::Unknown);
        assert_eq!(enter_outcome(Err(take)), PtyEnter::Unknown);
    }

    #[test]
    fn oversized_payload_is_refused_before_any_attach() {
        let bytes = vec![b'x'; sot_log::fe_client::TAKE_QUEUE_CAP + 1];
        let err = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, deadline())
            .expect_err("oversized payload must be refused");
        assert_eq!(err.phase, "size");
        assert!(!err.submitted);
    }

    #[test]
    fn exactly_the_cap_is_not_oversized() {
        // The cap itself is legal — only `CAP + 1` is refused. This
        // would attempt a real attach (and fail on the nonexistent path
        // some other way), which is enough to prove the size gate did
        // NOT reject it — that failure is expected and not asserted on
        // further than "it is not the size-gate error."
        let bytes = vec![b'x'; sot_log::fe_client::TAKE_QUEUE_CAP];
        let err = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, deadline())
            .expect_err("a nonexistent state dir cannot succeed");
        assert_ne!(err.phase, "size", "the cap itself must not trip the size gate");
    }

    #[test]
    fn empty_payload_succeeds_with_no_attach() {
        let n = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &[], deadline())
            .expect("an empty payload succeeds trivially, with nothing to attach for");
        assert_eq!(n, 0);
    }

    #[test]
    fn write_and_enter_oversized_payload_is_refused_before_any_attach() {
        let bytes = vec![b'x'; sot_log::fe_client::TAKE_QUEUE_CAP + 1];
        let budget = Duration::from_secs(5);
        let err = write_and_enter(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, budget, budget, budget)
            .expect_err("oversized payload must be refused");
        assert_eq!(err.phase, "size");
        assert!(!err.submitted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `source` without its `#[cfg(test)]` modules and its comment lines.
    /// A module ends at the first `}` line at its own indentation; counting
    /// braces would miscount the ones inside string literals.
    fn without_test_modules(source: &str) -> String {
        let mut out = String::new();
        let mut lines = source.lines().peekable();
        while let Some(line) = lines.next() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            let next_is_mod = lines.peek().is_some_and(|next| {
                let next = next.trim_start();
                next.starts_with("mod ") || next.starts_with("pub mod ") || next.starts_with("pub(crate) mod ")
            });
            if trimmed == "#[cfg(test)]" && next_is_mod {
                let header = lines.next().unwrap_or_default();
                if header.trim_end().ends_with(';') || header.trim_end().ends_with('}') {
                    continue;
                }
                let close = format!("{}}}", &header[..header.len() - header.trim_start().len()]);
                for skipped in lines.by_ref() {
                    if skipped.trim_end() == close {
                        break;
                    }
                }
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    // #8: every run start passes the gate. The compiler enumerates spawns
    // through `spawn_detached_supervisor`'s permit parameter; this pins
    // what it cannot see — that the one reset is behind the gate, that
    // every spawn call hands a permit, and that a closing gate refuses
    // before anything is located or dialled.
    #[test]
    fn every_run_start_path_takes_a_permit() {
        let mut faults = Vec::new();

        let reg = crate::workspaces::Workspaces::new();
        assert!(reg.close_gate_and_settle(std::time::Instant::now()));
        let root = tempfile::tempdir().unwrap();
        let refusal = "workspace ws-gate-1 cannot start: this computer's backend is shutting down".to_string();
        let started = start_supervisor(
            root.path(), "ws-gate-1", StartMode::Start, &["true".to_string()], root.path(), "", "gate", reg.clone(),
        );
        if started != Err(refusal.clone()) {
            faults.push(format!("start_supervisor with the gate closing answered {started:?}"));
        }
        let reset = reset_run(&reg, "ws-gate-1", &state_dir_for(root.path(), "ws-gate-1"));
        if reset != Err(refusal.clone()) {
            faults.push(format!("reset_run with the gate closing answered {reset:?}"));
        }

        let reset_needle = "supervisor_client::reset(";
        let spawn_needle = "spawn_detached_supervisor(";
        let mut resets = 0;
        let mut spawn_calls = 0;
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        let mut pending = vec![src.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        let mut files_read = 0;
        for path in files {
            // Test files have no `#[cfg(test)] mod` wrapper for `without_test_modules` to strip.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let in_tests_folder = path.strip_prefix(&src).is_ok_and(|rel| {
                rel.parent().is_some_and(|dirs| dirs.components().any(|c| c.as_os_str() == "tests"))
            });
            if in_tests_folder
                || name.ends_with("_tests.rs")
                || name == "tests.rs"
                || name == "test_support.rs"
                || name.starts_with("tests_")
                || name.contains("_tests_")
            {
                continue;
            }
            files_read += 1;
            let text = without_test_modules(&std::fs::read_to_string(&path).unwrap());
            for (pos, _) in text.match_indices(reset_needle) {
                resets += 1;
                let enclosing = text[..pos].rfind("fn ").map(|at| &text[at + 3..]).unwrap_or("");
                if !enclosing.starts_with("reset_run(") {
                    faults.push(format!("{}: supervisor_client::reset called outside reset_run", path.display()));
                }
            }
            for (pos, _) in text.match_indices(spawn_needle) {
                let args = text[pos + spawn_needle.len()..].trim_start();
                if text[..pos].ends_with("fn ") {
                    if !args.starts_with("_permit: &StartPermit,") {
                        faults.push(format!("{}: spawn_detached_supervisor does not take a permit first", path.display()));
                    }
                    continue;
                }
                spawn_calls += 1;
                let first = args.split(',').next().unwrap_or("");
                if !first.contains("permit") {
                    faults.push(format!("{}: a spawn_detached_supervisor call passes {first:?} first", path.display()));
                }
            }
        }
        if files_read == 0 {
            faults.push(format!("the scan read no source files under {}", src.display()));
        }
        if resets != 1 {
            faults.push(format!("supervisor_client::reset occurs {resets} times outside tests, not once"));
        }
        if spawn_calls < 2 {
            faults.push(format!("found {spawn_calls} spawn_detached_supervisor calls; the scan is not seeing the spawns"));
        }
        assert!(faults.is_empty(), "{faults:#?}");
    }

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
        sot_log::pointer::publish(state_dir, voyage_id).expect("publish the pointer");
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
        let holder = sot_log::fence::lock_supervisor(state_dir).expect("take the fence");
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
    // report (Windows' reparse-point check, `fsutil.rs`) sharing the SAME
    // `Error::State` shape as genuine contention. `is_lock_contention` is
    // the pure predicate that tells them apart (Codex review,
    // 2026-09-11); this is its regression test -- pure string matching,
    // independent of any real lock file, but `is_lock_contention` itself
    // lives inside `mod runtime`, gated like every other function this
    // module's tests reach.
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


    #[test]
    fn phase_str_is_total_and_snake_case() {
        use sot_log::wire::SupervisorPhase;
        assert_eq!(phase_str(SupervisorPhase::Starting), "starting");
        assert_eq!(phase_str(SupervisorPhase::Ready), "ready");
        assert_eq!(phase_str(SupervisorPhase::Ending), "ending");
        assert_eq!(phase_str(SupervisorPhase::EndedNoRespawn), "ended_no_respawn");
        assert_eq!(phase_str(SupervisorPhase::Terminal), "terminal");
    }

    #[test]
    fn never_started_phase_is_distinct_from_unreachable_and_every_answered_phase() {
        // First live shakedown fix: a capsule workspace nobody has ever
        // started must read as quietly "stopped", not as the loud
        // "unreachable" a query FAILURE reports — the two must never
        // collide with each other or with any answered lifecycle phase.
        use sot_log::wire::SupervisorPhase;
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
        use sot_log::wire::SupervisorPhase;
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

    #[test]
    fn restart_budget_numbers_match_adr_0041s_own_launcher_table() {
        assert_eq!(RESTART_BACKOFFS.len(), 5);
        assert_eq!(MAX_RESTARTS_PER_WINDOW, 5);
        assert_eq!(RESTART_WINDOW, std::time::Duration::from_secs(60));
        assert_eq!(
            RESTART_BACKOFFS.map(|d| d.as_secs()),
            [1, 3, 7, 15, 30]
        );
    }
}
