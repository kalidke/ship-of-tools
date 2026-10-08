//! The authority's own state, its command handling and the wire mapping.

use super::*;

pub(super) mod lane;

#[cfg(windows)]
pub(super) fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};
    unsafe {
        let pid = GetCurrentProcessId();
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        if GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let created = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        Ok((pid, created))
    }
}

/// ADR 0043 decision 21: the same `(pid, start-time ticks)` pair
/// `capsule/mod.rs`'s own `self_status` uses for the identical adoption-
/// challenge identity (decision 16) — Linux has no `GetProcessTimes`
/// analogue, so this reads `/proc/self/stat` via the same helper
/// `challenge_unix` already exposes for reading a PEER's start time.
#[cfg(target_os = "linux")]
pub(super) fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    Ok((std::process::id(), crate::identity::challenge_unix::self_start_ticks()?))
}

/// ADR 0043 decision 21, the macOS arm — the exact twin of what
/// `capsule/mod.rs`'s own `self_status` reports on this target, for the
/// identical adoption-challenge identity (decision 16), and it is NOT a
/// start time. `created` is per-platform by definition ("whatever unit
/// this OS's own `status_ok.created` carries, compared for equality
/// only" — `client::PeerIdentity::created`), and on macOS that unit is
/// the kernel's `pidversion`: `challenge_macos`'s step 5 compares a
/// reply's `created` against the pidversion it read out of the peer's
/// audit token. A start time here would type-check, carry a
/// plausible-looking number, and make every macOS adoption challenge
/// `Foreign`. `self_pidversion` is the self-facing twin that exists for
/// exactly this call site, the way `self_start_ticks` is on Linux.
///
/// The pid is pinned against reuse by the pidversion, not by the pid:
/// the generation counter is the kernel's own and monotonic per process
/// INSTANCE, so a recycled pid carries a different one.
#[cfg(target_os = "macos")]
pub(super) fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    Ok((std::process::id(), u64::from(crate::identity::challenge_macos::self_pidversion()?)))
}

// ---------------------------------------------------------------------
// The supervisor lane's own command/query/status handling
// ---------------------------------------------------------------------

/// Everything the lane's own command/query/status handling needs —
/// deliberately separate from the main loop's own `Lifecycle` so the
/// borrow-checker never has to reason about both at once inside one
/// giant function. `voyage_id` is `None` only during `Recovering` (
/// pointer discovery has not happened yet) — every state that can admit
/// a voyage-fenced command is reached strictly AFTER it becomes `Some`
/// and never reverts to `None`.
/// `stop` no longer transitions the
/// `Lifecycle` at all — an earlier `Lifecycle::Stopping` variant
/// discarded whatever worker/receiver was in flight (retaining only a
/// bare `JoinHandle`, unable to preserve its actual RESULT), which is
/// exactly how a `Fatal` outcome from a Stop-preempted Reset/EndRun
/// could be silently dropped and the process still exit 0. Instead, the
/// underlying `Lifecycle` is left COMPLETELY ALONE to keep resolving
/// itself through its own normal, already-correct transition arms
/// (worker ownership, panic detection, watchdogs — all unchanged); this
/// struct is the ONLY thing Stop's acceptance actually records, and the
/// main loop's own exit condition (in `supervise_inner`) reads it back
/// to decide when the underlying Lifecycle has reached a resting point
/// worth exiting from.
pub(super) struct StopRequested {
    /// The FIRST connection whose `stop` was accepted — the one the main
    /// loop's exit condition waits to see fully delivered-or-given-up
    /// (via the SAME per-connection `PendingClose` gate every other
    /// reply already uses) before it actually breaks the loop. A SECOND
    /// (or Nth) `stop` from a DIFFERENT connection still gets its own
    /// honest, independently delivery-gated reply — see
    /// `handle_lane_bytes`'s own `CommandEffect::Stop` arm — but never
    /// re-arms this: "a second Stop... is answered from the existing
    /// gate, idempotent, never re-arming it."
    pub(super) primary_conn: ConnId,
    /// MONOTONIC once `true` (never reset to `false`): OR'd from
    /// "already `Terminal`, or THIS Stop's own `journal::finish` itself
    /// failed" at the moment of acceptance, together with every
    /// SUBSEQUENT admitted Stop's own outcome. The final exit code
    /// additionally checks whether the underlying `Lifecycle` reached
    /// `Terminal` on its own account by the time the loop actually
    /// exits — computed fresh, never stored here, because sticky
    /// `Terminal` is already the `Lifecycle`'s own invariant.
    pub(super) terminal_severity: bool,
}

pub(super) struct AuthorityState {
    pub(super) state_dir: PathBuf,
    pub(super) voyage_id: Option<String>,
    pub(super) self_pid: u32,
    pub(super) self_created: u64,
    pub(super) stop_requested: Option<StopRequested>,
    /// legs whose `Lifecycle` state was left
    /// (`Ready`/`Ending` resolving to something else) while STILL ALIVE —
    /// [`reap_leg_if_already_exited`]'s old design only ever handled
    /// "already exited by the time its state is left"; a leg that exits a
    /// MOMENT LATER (e.g. `Ending` resolving `PreBarrierFailed`: the
    /// writer released its lock, proving the marker check can proceed,
    /// before the process itself has actually exited) had no owner left
    /// at all once its retained handle was silently dropped — a zombie
    /// for the supervisor's whole remaining lifetime. The owner reaps;
    /// ownership ends only at an OBSERVED death, never merely at the
    /// Lifecycle transition that happens to coincide with it. Every site
    /// that used to call `reap_leg_if_already_exited` now calls
    /// [`retire_leg`] instead: reap immediately if `wait(Duration::ZERO)`
    /// already confirms the exit, otherwise the handle MOVES here rather
    /// than being dropped. [`reap_retired_legs`] polls this once per main-
    /// loop tick (the existing `MAIN_LOOP_POLL` cadence — no new timer).
    /// A leg that never dies stays here for the supervisor's own
    /// lifetime — outliving it (ADR 0043 decision 14's detached kill
    /// domain) is by design; the vector is bounded by the number of legs
    /// this authority ever spawned, never unbounded. One shape on both
    /// platforms (not `cfg(target_os = "linux")`-gated): a Windows
    /// `Process` handle has no reap concept, so an entry there just sits
    /// until its own `wait` confirms exit (near-immediate — Windows has
    /// no zombie/reap delay at all) and is then dropped by
    /// `reap_retired_legs`'s own `retain`, its `Drop` (`CloseHandle`)
    /// doing the only cleanup that platform needs.
    pub(super) retired_legs: Vec<LegProcess>,
}

/// What `handle_command` decided to do — the CALLER (`handle_lane_bytes`)
/// applies the resulting effect INLINE, before processing any further
/// frame in the same read.
pub(super) enum CommandEffect {
    /// Begin ending the current `Ready` leg. The wire reply is DEFERRED
    /// to `record_closed` — this variant carries no reply value at
    /// all; `Accepted` is never sent, only implied.
    EndRun { operation_id: String, epoch: Option<u64>, reason: String },
    /// Begin a reset (admissible only from `EndedNoRespawn` — checked by
    /// the caller before this effect is ever produced). No `reply` field:
    /// a freshly
    /// admitted reset is ALWAYS `Accepted` — every OTHER outcome for
    /// this operation id (a digest conflict, an already-known id) is
    /// already intercepted earlier in `handle_command`, before this
    /// variant is ever constructed — so a stored value here could only
    /// ever equal the one constant `handle_lane_bytes` can just write
    /// directly.
    Reset { operation_id: String, new_voyage: String, aside: Option<String> },
    /// `stop` was accepted and already durably journaled. No
    /// separate `journal_ok` field: it duplicated exactly `reply`'s own shape
    /// within this variant — `journal::finish` succeeding is the ONLY
    /// way `reply` becomes `Stopping` here, and failing is the ONLY way
    /// it becomes `Failed` — so the caller reads `journal::finish`'s
    /// own outcome directly off `reply` (`matches!(reply,
    /// SupervisorOperationState::Failed { .. })`) instead of a second,
    /// redundant bool always in lockstep with it.
    Stop { reply: SupervisorOperationState },
}

fn reset_refusal_detail(lifecycle: &Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Recovering { .. } => "the authority is still recovering from a prior run",
        Lifecycle::InitialProbe { .. } => "the authority is still determining whether a leg is already live",
        Lifecycle::Spawning { .. } => "a leg is currently being spawned",
        Lifecycle::Ready { .. } => "a leg is currently live; end the run before resetting",
        Lifecycle::Ending { .. } => "an end_run is currently in progress",
        Lifecycle::Resetting { .. } => "a reset is already in progress",
        Lifecycle::EndedNoRespawn => "reset is admissible here — this refusal should be unreachable",
        Lifecycle::Terminal { .. } => "the authority is in a terminal state",
        Lifecycle::StorageFull(_) => "the authority is waiting for storage",
    }
}

/// Publishes `record`, or, when that fails, admits the `.active` record
/// already on disk with the same digest: a `journal::begin` can fail after its
/// rename took (a directory flush on a full volume), and the record then
/// exists with no worker behind it. Returns the record the operation runs
/// from; nothing is minted again.
fn begin_or_readback(state_dir: &Path, operation_id: &str, record: journal::ActiveRecord) -> crate::Result<journal::ActiveRecord> {
    match journal::begin(state_dir, operation_id, &record) {
        Ok(()) => Ok(record),
        Err(e) => match journal::read_active(state_dir, operation_id) {
            Ok(Some(existing)) if existing.digest == record.digest => Ok(existing),
            _ => Err(e),
        },
    }
}

impl AuthorityState {
    /// `status` and `query` only — never `command`, which
    /// `handle_lane_bytes` calls directly through [`Self::handle_command`]
    /// so it can apply the resulting [`CommandEffect`]'s `Lifecycle`
    /// transition INLINE, before processing any further frame in the
    /// same read (per-frame admissibility against the CURRENT lifecycle).
    fn handle_status_or_query(&mut self, lifecycle: &Lifecycle, req: SupervisorRequest) -> SupervisorReply {
        match req {
            SupervisorRequest::Hello { .. } | SupervisorRequest::Command { .. } => {
                unreachable!("handled by the caller before this is reached")
            }
            SupervisorRequest::Status => SupervisorReply::StatusOk {
                pid: self.self_pid,
                created: self.self_created,
                voyage: self.voyage_id.clone(),
                leg: match lifecycle {
                    Lifecycle::Ready { .. } | Lifecycle::Ending { .. } => {
                        self.voyage_id.as_deref().and_then(|v| leg_epoch_of(&self.state_dir, v))
                    }
                    _ => None,
                },
                phase: lifecycle.wire_phase(),
            },
            SupervisorRequest::Query { operation_id } => SupervisorReply::Operation(self.query_state(&operation_id)),
        }
    }

    /// `Err` is a plain reply with no transition (a refusal, a
    /// query-style answer, or a query-time journal error — the latter a
    /// LOUD STOP, matched via [`is_journal_unreadable`]). `Ok` is a
    /// [`CommandEffect`] the caller applies immediately.
    fn handle_command(
        &mut self,
        lifecycle: &Lifecycle,
        operation_id: String,
        op: SupervisorOp,
    ) -> Result<CommandEffect, SupervisorOperationState> {
        let digest = match digest_of(&op) {
            Ok(d) => d,
            Err(e) => return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("{e}")) }),
        };

        // resolve an EXISTING operation id
        // BEFORE voyage fencing, for every command family — an earlier
        // version fenced FIRST, so replaying a SUCCESSFUL Reset's own
        // id/digest, still fenced to the voyage it changed FROM, would
        // hit `stale_voyage` (the CURRENT voyage has already moved on)
        // instead of reading back its own stored `ResetDone`. An ACTIVE
        // entry with a matching digest, or an id that has reached ANY
        // OTHER journal state at all, answers idempotently from that
        // state; only a genuinely UNKNOWN id ever reaches fencing.
        let mut from_record = None;
        match journal::read_active(&self.state_dir, &operation_id) {
            Ok(Some(existing)) if existing.digest != digest => {
                return Err(SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::IdConflict });
            }
            // An active entry with this digest: answered from its state, unless
            // it is still Accepted and no worker in this process runs it (its
            // `begin` failed after the rename took, or a worker's storage
            // failure left it active): then it is admitted from its record.
            Ok(Some(existing)) => {
                let state = self.query_state(&operation_id);
                if matches!(state, SupervisorOperationState::Accepted) && !self.worker_runs(lifecycle, &operation_id, &existing.op) {
                    from_record = Some(existing);
                } else {
                    return Err(state);
                }
            }
            Ok(None) => {}
            Err(e) => {
                return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) })
            }
        }
        if from_record.is_none() {
            let existing = self.query_state(&operation_id);
            if !matches!(existing, SupervisorOperationState::UnknownOperation) {
                return Err(existing); // idempotent resubmit, already terminal/closed
            }
        }

        // Voyage-fencing (ADR 0041): a mismatch is `refused
        // {stale_voyage}` with NO MUTATION — reached only once this id
        // is confirmed genuinely new. `Reset{voyage: None}` is legal
        // ONLY when there is truly no live voyage to fence against —
        // never true once `voyage_id` is `Some` (which every state able
        // to ADMIT a reset requires).
        let fenced_ok = match &op {
            SupervisorOp::EndRun { voyage, .. } => self.voyage_id.as_deref() == Some(voyage.as_str()),
            SupervisorOp::Reset { voyage: Some(v) } => self.voyage_id.as_deref() == Some(v.as_str()),
            SupervisorOp::Reset { voyage: None } => self.voyage_id.is_none(),
            SupervisorOp::Stop => true,
        };
        if !fenced_ok {
            return Err(SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::StaleVoyage });
        }

        match op {
            SupervisorOp::EndRun { reason, .. } => self.admit_end_run(lifecycle, operation_id, digest, reason, from_record),
            SupervisorOp::Reset { .. } => self.admit_reset(lifecycle, operation_id, digest, from_record),
            SupervisorOp::Stop => self.admit_stop(operation_id, digest, from_record),
        }
    }

    /// Whether a worker in THIS process runs `operation_id`: its `end_run` or
    /// reset is the one the lifecycle carries, or a stop was accepted. An
    /// operation with an active record and no worker is admitted from it.
    fn worker_runs(&self, lifecycle: &Lifecycle, operation_id: &str, op: &journal::ActiveOp) -> bool {
        match (op, lifecycle) {
            (journal::ActiveOp::EndRun { .. }, Lifecycle::Ending { operation_id: running, .. })
            | (journal::ActiveOp::Reset { .. }, Lifecycle::Resetting { operation_id: running, .. }) => {
                running == operation_id
            }
            (journal::ActiveOp::Stop, _) => self.stop_requested.is_some(),
            _ => false,
        }
    }

    /// Publishes `record` for a new operation, or hands back the active record
    /// this operation is admitted from (`from_record`, or the one a failed
    /// `journal::begin` left on disk with this digest).
    fn admit(
        &self,
        operation_id: &str,
        record: journal::ActiveRecord,
        from_record: Option<journal::ActiveRecord>,
    ) -> Result<journal::ActiveRecord, SupervisorOperationState> {
        match from_record {
            Some(existing) => Ok(existing),
            None => begin_or_readback(&self.state_dir, operation_id, record).map_err(|e| {
                SupervisorOperationState::Failed { detail: bounded_detail(format!("journal begin failed: {e}")) }
            }),
        }
    }

    fn admit_end_run(
        &self,
        lifecycle: &Lifecycle,
        operation_id: String,
        digest: String,
        reason: String,
        from_record: Option<journal::ActiveRecord>,
    ) -> Result<CommandEffect, SupervisorOperationState> {
        if !matches!(lifecycle, Lifecycle::Ready { .. }) {
            return Err(SupervisorOperationState::Failed { detail: bounded_detail("no leg is currently running") });
        }
        let voyage_id = self.voyage_id.clone().expect("fenced_ok already confirmed a voyage_id");
        let epoch = leg_epoch_of(&self.state_dir, &voyage_id);
        let record = journal::ActiveRecord {
            operation_id: operation_id.clone(),
            digest,
            op: journal::ActiveOp::EndRun { voyage: voyage_id, epoch },
        };
        match self.admit(&operation_id, record, from_record)?.op {
            journal::ActiveOp::EndRun { epoch, .. } => Ok(CommandEffect::EndRun { operation_id, epoch, reason }),
            _ => Err(record_mismatch(&operation_id)),
        }
    }

    fn admit_reset(
        &self,
        lifecycle: &Lifecycle,
        operation_id: String,
        digest: String,
        from_record: Option<journal::ActiveRecord>,
    ) -> Result<CommandEffect, SupervisorOperationState> {
        if !matches!(lifecycle, Lifecycle::EndedNoRespawn) {
            return Err(SupervisorOperationState::Failed { detail: bounded_detail(reset_refusal_detail(lifecycle)) });
        }
        let admitted = match from_record {
            Some(existing) => existing,
            None => {
                let new_voyage = uuid::Uuid::now_v7().to_string();
                let aside = Some(mint_aside_name().map_err(|e| SupervisorOperationState::Failed {
                    detail: bounded_detail(format!("{e}")),
                })?);
                let record = journal::ActiveRecord {
                    operation_id: operation_id.clone(),
                    digest,
                    op: journal::ActiveOp::Reset { old_voyage: self.voyage_id.clone(), new_voyage, aside },
                };
                self.admit(&operation_id, record, None)?
            }
        };
        match admitted.op {
            journal::ActiveOp::Reset { new_voyage, aside, .. } => Ok(CommandEffect::Reset { operation_id, new_voyage, aside }),
            _ => Err(record_mismatch(&operation_id)),
        }
    }

    fn admit_stop(
        &self,
        operation_id: String,
        digest: String,
        from_record: Option<journal::ActiveRecord>,
    ) -> Result<CommandEffect, SupervisorOperationState> {
        let record = journal::ActiveRecord { operation_id: operation_id.clone(), digest, op: journal::ActiveOp::Stop };
        self.admit(&operation_id, record, from_record)?;
        let t = journal::TerminalRecord::Stopping;
        match journal::finish(&self.state_dir, &operation_id, &t) {
            Ok(()) => Ok(CommandEffect::Stop { reply: terminal_to_wire(t) }),
            Err(e) => Ok(CommandEffect::Stop {
                reply: SupervisorOperationState::Failed { detail: bounded_detail(format!("journal finish failed: {e}")) },
            }),
        }
    }

    fn query_state(&self, operation_id: &str) -> SupervisorOperationState {
        match journal::read_terminal(&self.state_dir, operation_id) {
            Ok(Some(t)) => return terminal_to_wire(t),
            Ok(None) => {}
            Err(e) => return SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) },
        }
        match journal::is_closed(&self.state_dir, operation_id) {
            Ok(true) => return SupervisorOperationState::RecordClosed,
            Ok(false) => {}
            Err(e) => return SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) },
        }
        match journal::read_active(&self.state_dir, operation_id) {
            Ok(Some(_)) => SupervisorOperationState::Accepted,
            Ok(None) => SupervisorOperationState::UnknownOperation,
            Err(e) => SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) },
        }
    }
}

/// An active record whose digest matches but whose operation is another kind: a corrupt journal.
fn record_mismatch(operation_id: &str) -> SupervisorOperationState {
    SupervisorOperationState::Failed {
        detail: bounded_detail(format!("the active record of {operation_id} is not this operation's kind")),
    }
}

fn terminal_to_wire(t: journal::TerminalRecord) -> SupervisorOperationState {
    match t {
        journal::TerminalRecord::RecordVerified => SupervisorOperationState::RecordVerified,
        journal::TerminalRecord::ResetDone { new_voyage } => SupervisorOperationState::ResetDone { new_voyage },
        journal::TerminalRecord::Stopping => SupervisorOperationState::Stopping,
        journal::TerminalRecord::Failed { detail } => SupervisorOperationState::Failed { detail: bounded_detail(detail) },
    }
}

/// A journal-read failure surfacing all the way up to the main loop is a
/// LOUD STOP — `true` iff `reply`'s own detail text names one
/// (`query_state`/`handle_command`'s own "journal unreadable: " prefix,
/// minted nowhere else in this module).
pub(super) fn is_journal_unreadable(reply: &SupervisorOperationState) -> bool {
    matches!(reply, SupervisorOperationState::Failed { detail } if detail.starts_with("journal unreadable: "))
}

pub(super) fn encode_reply_or_fallback(reply: &SupervisorReply) -> Vec<u8> {
    wire::encode_supervisor_reply(reply).unwrap_or_else(|e| {
        note(format_args!("a reply failed to encode ({e}); substituting a minimal failure reply"));
        wire::encode_supervisor_reply(&SupervisorReply::Operation(SupervisorOperationState::Failed {
            detail: "internal error".into(),
        }))
        .expect("this minimal fallback reply is always encodable")
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_journal_unreadable_matches_only_that_shape() {
        assert!(is_journal_unreadable(&SupervisorOperationState::Failed { detail: "journal unreadable: boom".into() }));
        assert!(!is_journal_unreadable(&SupervisorOperationState::Failed { detail: "record_append".into() }));
        assert!(!is_journal_unreadable(&SupervisorOperationState::Accepted));
    }

    /// Every non-`EndedNoRespawn` state must have SOME refusal detail
    /// -- `reset_refusal_detail`'s own match is exhaustive over
    /// every OTHER variant at compile time; this exercises a
    /// representative sample of the actual strings too. No `Stopping`
    /// case any more: `stop` no longer
    /// touches the `Lifecycle` at all, so there is no "busy because
    /// stopping" state for `reset_refusal_detail` to describe -- a
    /// reset attempted while a stop is pending is admitted and resolved
    /// exactly like any other command would be.
    #[test]
    fn reset_refusal_detail_names_the_reason_for_every_busy_state() {
        let (_tx, rx) = mpsc::channel::<RecoveryOutcome>();
        let recovering = Lifecycle::Recovering { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now(), first_leg: true };
        assert!(reset_refusal_detail(&recovering).contains("recovering"));

        let terminal = Lifecycle::Terminal { detail: "x".into(), entered_at: Instant::now() };
        assert!(reset_refusal_detail(&terminal).contains("terminal"));
    }

    fn test_authority(state_dir: &Path) -> AuthorityState {
        AuthorityState {
            state_dir: state_dir.to_path_buf(),
            voyage_id: None,
            self_pid: 0,
            self_created: 0,
            stop_requested: None,
            retired_legs: Vec::new(),
        }
    }

    /// A reset whose `.active` record was published but which no worker here
    /// runs (a `journal::begin` that failed after its rename took, or a
    /// worker's storage failure that left it active) is admitted from that
    /// record: its own voyage and aside, nothing minted again.
    #[test]
    fn a_published_reset_without_its_worker_is_admitted_from_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let old_voyage = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let mut authority = test_authority(dir.path());
        authority.voyage_id = Some(old_voyage.clone());

        let op = SupervisorOp::Reset { voyage: Some(old_voyage.clone()) };
        let new_voyage = uuid::Uuid::now_v7().to_string();
        let aside = "drawer.voyage.reset-aaaaaaaaaaaaaaaa".to_string();
        let record = journal::ActiveRecord {
            operation_id: "reset-x".into(),
            digest: digest_of(&op).unwrap(),
            op: journal::ActiveOp::Reset {
                old_voyage: Some(old_voyage),
                new_voyage: new_voyage.clone(),
                aside: Some(aside.clone()),
            },
        };
        journal::begin(dir.path(), "reset-x", &record).unwrap();

        let effect = authority
            .handle_command(&Lifecycle::EndedNoRespawn, "reset-x".into(), op)
            .expect("an active reset with no worker is admitted from its record");
        let CommandEffect::Reset { operation_id, new_voyage: admitted, aside: admitted_aside } = effect else {
            panic!("expected a Reset effect");
        };
        assert_eq!(operation_id, "reset-x");
        assert_eq!(admitted, new_voyage, "the record's own voyage, not a newly minted one");
        assert_eq!(admitted_aside.as_deref(), Some(aside.as_str()));
    }

    /// a SUCCESSFUL Reset's own operation id,
    /// resubmitted with the SAME digest AFTER the voyage it changed
    /// FROM no longer matches the current one, must answer with the
    /// stored `ResetDone` — not `refused{stale_voyage}`. Drives
    /// `handle_command` directly (pure logic + journal I/O, no OS
    /// process needed) rather than through a real lane connection.
    #[test]
    fn a_completed_reset_replayed_after_the_voyage_moved_on_is_idempotent_not_stale_voyage() {
        let dir = tempfile::tempdir().unwrap();
        let old_voyage = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let mut authority = test_authority(dir.path());
        authority.voyage_id = Some(old_voyage.clone());

        let op = SupervisorOp::Reset { voyage: Some(old_voyage.clone()) };
        let effect = authority
            .handle_command(&Lifecycle::EndedNoRespawn, "reset-1".into(), op.clone())
            .expect("a fresh reset from EndedNoRespawn is admitted");
        let CommandEffect::Reset { operation_id, new_voyage, aside } = effect else {
            panic!("expected a Reset effect");
        };
        reset_pointer(dir.path(), &new_voyage, aside.as_deref()).unwrap();
        journal::finish(dir.path(), &operation_id, &journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() })
            .unwrap();

        // The authority's own voyage_id has now moved on (as it would
        // in the main loop, once Resetting concludes) -- replaying the
        // SAME id+digest, still naming the OLD voyage, must NOT be
        // fenced against the NEW one.
        authority.voyage_id = Some(new_voyage.clone());
        let replay = authority.handle_command(&Lifecycle::EndedNoRespawn, "reset-1".into(), op);
        match replay {
            Err(SupervisorOperationState::ResetDone { new_voyage: replayed }) => assert_eq!(replayed, new_voyage),
            Ok(_) => panic!("expected an idempotent Err(ResetDone), got a fresh CommandEffect instead"),
            Err(other) => panic!("expected Err(ResetDone), got {other:?}"),
        }
    }

    /// `terminal_severity` is MONOTONIC —
    /// computed as `prior || new`, never reassigned from scratch. The
    /// regression this guards: the FIRST stop is accepted while the
    /// authority is genuinely `Terminal` (severity forced `true`); by
    /// the time a SECOND, entirely ordinary stop arrives, the
    /// underlying `Lifecycle` is untouched by Stop so
    /// it is STILL `Terminal` in reality — but this proves the
    /// bookkeeping itself never depends on that, by exercising the
    /// second stop against a DIFFERENT, non-Terminal lifecycle and a
    /// cleanly-succeeding journal write (`terminal_now` computes
    /// `false` on its own): the OR must still leave `terminal_severity`
    /// `true`, exactly the `|=`, never `=`, `handle_lane_bytes`'s own
    /// `CommandEffect::Stop` arm applies.
    #[test]
    fn stop_requested_terminal_severity_is_monotonic_across_repeated_stops() {
        let dir = tempfile::tempdir().unwrap();
        let mut authority = test_authority(dir.path());
        let terminal = Lifecycle::Terminal { detail: "x".into(), entered_at: Instant::now() };

        let CommandEffect::Stop { reply } =
            authority.handle_command(&terminal, "stop-1".into(), SupervisorOp::Stop).unwrap()
        else {
            panic!("expected a Stop effect");
        };
        let journal_failed = matches!(reply, SupervisorOperationState::Failed { .. });
        assert!(!journal_failed);
        // Mirror what handle_lane_bytes does with the effect: this is
        // the FIRST stop, accepted while already Terminal.
        authority.stop_requested = Some(StopRequested {
            primary_conn: 0, // ConnId is a bare u64; no real connection needed for this test
            terminal_severity: matches!(terminal, Lifecycle::Terminal { .. }) || journal_failed,
        });
        assert!(authority.stop_requested.as_ref().unwrap().terminal_severity);

        // A SECOND stop, a DIFFERENT id, against a NON-Terminal
        // lifecycle, whose own journal write succeeds cleanly -- this
        // stop's OWN severity computes `false` on its own; the stored
        // flag must still not clear.
        let not_terminal = Lifecycle::EndedNoRespawn;
        let CommandEffect::Stop { reply: second_reply } =
            authority.handle_command(&not_terminal, "stop-2".into(), SupervisorOp::Stop).unwrap()
        else {
            panic!("expected a Stop effect");
        };
        let second_journal_failed = matches!(second_reply, SupervisorOperationState::Failed { .. });
        assert!(!second_journal_failed);
        let terminal_now = matches!(not_terminal, Lifecycle::Terminal { .. }) || second_journal_failed;
        assert!(!terminal_now, "test setup: the second stop alone must look clean, or this proves nothing");
        authority.stop_requested.as_mut().unwrap().terminal_severity |= terminal_now;
        assert!(
            authority.stop_requested.as_ref().unwrap().terminal_severity,
            "a clean second stop against a non-terminal lifecycle must never clear the first stop's own terminal severity"
        );
    }
}
