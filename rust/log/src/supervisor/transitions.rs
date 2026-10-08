//! One tick of each Lifecycle state: supervise_inner's main loop calls the function for the current state
//! and stores the state it returns.

use super::*;

pub(super) fn advance_recovering(rx: mpsc::Receiver<RecoveryOutcome>, handle: JoinHandle<()>, started_at: Instant, config: &SuperviseConfig, authority: &mut AuthorityState, now: Instant) -> Lifecycle {
    match rx.try_recv() {
        Ok(RecoveryOutcome::Done { voyage_id, ended }) => {
            join_and_warn(handle, "recovery");
            authority.voyage_id = Some(voyage_id.clone());
            if ended {
                Lifecycle::EndedNoRespawn
            } else {
                let voyage_root = voyage_root_path(&config.state_dir, &voyage_id);
                let (rx, handle) = spawn_initial_probe(voyage_id, voyage_root);
                Lifecycle::InitialProbe { rx, handle, started_at: now }
            }
        }
        Ok(RecoveryOutcome::Storage(detail)) => {
            join_and_warn(handle, "recovery");
            note(format_args!("recovery met storage exhaustion ({detail}); waiting for storage"));
            Lifecycle::StorageFull(storage::Wait::new(storage::Resume::Recover))
        }
        Ok(RecoveryOutcome::Fatal { detail }) => {
            join_and_warn(handle, "recovery");
            Lifecycle::Terminal { detail, entered_at: now }
        }
        Err(mpsc::TryRecvError::Empty) => {
            if watchdog_expired(started_at, RECOVERY_WATCHDOG, now) {
                abandon_worker(handle, "recovery");
                Lifecycle::Terminal { detail: "recovery operation watchdog expired".into(), entered_at: now }
            } else {
                Lifecycle::Recovering { rx, handle, started_at }
            }
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            join_and_warn(handle, "recovery");
            Lifecycle::Terminal {
                detail: "the recovery thread ended without a result (possible panic)".into(),
                entered_at: now,
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn advance_initial_probe(rx: mpsc::Receiver<ProbeOutcome<LegProcess>>, handle: JoinHandle<()>, started_at: Instant, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &mut AuthorityState, now: Instant) -> Lifecycle {
    match rx.try_recv() {
        Ok(ProbeOutcome::Adopted(process)) => {
            join_and_warn(handle, "initial probe");
            authority.producer_ran = true;
            Lifecycle::Ready { process }
        }
        Ok(ProbeOutcome::Absent) => {
            join_and_warn(handle, "initial probe");
            let voyage_id = authority.voyage_id.clone().expect("set once Recovering completes");
            match should_spawn_after_absent(&config.state_dir, &voyage_id, config.mode) {
                Ok(true) => match lease.for_spawn() {
                    Ok(spawn_lease) => {
                        let voyage_root = voyage_root_path(&config.state_dir, &voyage_id);
                        // Stripped while no producer has run in this process
                        // (`leg_argv`), as the very first leg is.
                        let argv = leg_argv(config, authority.producer_ran, false);
                        let (rx, handle) = spawn_owned_spawn_attempt(
                            capsule_exe.to_path_buf(),
                            voyage_root,
                            voyage_id,
                            config.cols,
                            config.rows,
                            spawn_lease,
                            config.survival,
                            argv,
                        );
                        Lifecycle::Spawning { rx, handle, started_at: now }
                    }
                    Err(e) => Lifecycle::Terminal {
                        detail: bounded_detail(format!(
                            "could not prepare the parent-death lease for a fresh spawn: {e}"
                        )),
                        entered_at: now,
                    },
                },
                Ok(false) => Lifecycle::EndedNoRespawn,
                Err(e) => Lifecycle::Terminal {
                    detail: bounded_detail(format!("should_spawn_after_absent: {e}")),
                    entered_at: now,
                },
            }
        }
        Ok(ProbeOutcome::Foreign | ProbeOutcome::Wedged) => {
            join_and_warn(handle, "initial probe");
            Lifecycle::Terminal { detail: "the voyage pipe is foreign or unreachable at startup".into(), entered_at: now }
        }
        Ok(other) => {
            join_and_warn(handle, "initial probe");
            Lifecycle::Terminal {
                detail: bounded_detail(format!("unexpected probe_adopt_only outcome at startup: {other:?}")),
                entered_at: now,
            }
        }
        Err(mpsc::TryRecvError::Empty) => {
            if watchdog_expired(started_at, INITIAL_PROBE_WATCHDOG, now) {
                abandon_worker(handle, "initial probe");
                Lifecycle::Terminal { detail: "initial probe operation watchdog expired".into(), entered_at: now }
            } else {
                Lifecycle::InitialProbe { rx, handle, started_at }
            }
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            join_and_warn(handle, "initial probe");
            Lifecycle::Terminal {
                detail: "the initial probe thread ended without a result (possible panic)".into(),
                entered_at: now,
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn advance_spawning(rx: mpsc::Receiver<ProbeOutcome<LegProcess>>, handle: JoinHandle<()>, started_at: Instant, consecutive_unstable_legs: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &mut AuthorityState, now: Instant) -> Lifecycle {
    match rx.try_recv() {
        Ok(ProbeOutcome::Ready(process)) => {
            // No anti-flap accounting here at all — the counter
            // resets or increments ONLY once this leg's own
            // eventual death is observed and its recorded
            // producer_uptime_ms is read (`leg_was_stable`),
            // never on merely reaching Ready. An earlier version
            // zeroed the counter HERE, before any death could
            // ever be counted against a prior unstable run: a
            // leg that died moments after every respawn was
            // "the first" unstable leg forever — 90 legs in
            // ~120s of real Windows CI, the anti-flap bound
            // never tripping.
            join_and_warn(handle, "spawn");
            authority.producer_ran = true;
            Lifecycle::Ready { process }
        }
        Ok(ProbeOutcome::SpawnFailed(e)) => {
            join_and_warn(handle, "spawn");
            *consecutive_unstable_legs += 1;
            note(format_args!(
                "leg failed to spawn: {e} (unstable=true) consecutive_unstable_legs={consecutive_unstable_legs}"
            ));
            respawn_or_terminal(consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, true)
        }
        Ok(ProbeOutcome::LegEnded(status)) if storage::leg_death(status) == storage::LegDeath::Storage => {
            join_and_warn(handle, "spawn");
            note(format_args!("leg exited {} before reaching Ready (storage exhausted); waiting for storage", status_text(status)));
            Lifecycle::StorageFull(storage::Wait::new(storage::Resume::Respawn))
        }
        Ok(outcome @ (ProbeOutcome::KilledAfterTimeout | ProbeOutcome::LegEnded(_))) => {
            join_and_warn(handle, "spawn");
            *consecutive_unstable_legs += 1;
            let status = match outcome {
                ProbeOutcome::LegEnded(status) => status,
                _ => None,
            };
            note(format_args!(
                "leg ended status={} before reaching Ready (unstable=true) consecutive_unstable_legs={consecutive_unstable_legs}",
                status_text(status)
            ));
            respawn_or_terminal(consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, true)
        }
        Ok(ProbeOutcome::Foreign) => {
            // identity-
            // mismatched interference is an OPERATOR concern,
            // never counted as another unstable leg to respawn
            // over.
            join_and_warn(handle, "spawn");
            Lifecycle::Terminal {
                detail: "a foreign process answered the freshly spawned leg's own pipe".into(),
                entered_at: now,
            }
        }
        Ok(ProbeOutcome::KillOrWaitFailed(e)) => {
            join_and_warn(handle, "spawn");
            Lifecycle::Terminal { detail: bounded_detail(format!("kill/wait failed: {e}")), entered_at: now }
        }
        Ok(other) => {
            join_and_warn(handle, "spawn");
            Lifecycle::Terminal {
                detail: bounded_detail(format!("unexpected probe_owned_spawn outcome: {other:?}")),
                entered_at: now,
            }
        }
        Err(mpsc::TryRecvError::Empty) => {
            if watchdog_expired(started_at, SPAWNING_WATCHDOG, now) {
                abandon_worker(handle, "spawn");
                Lifecycle::Terminal { detail: "spawn operation watchdog expired".into(), entered_at: now }
            } else {
                Lifecycle::Spawning { rx, handle, started_at }
            }
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            join_and_warn(handle, "spawn");
            Lifecycle::Terminal { detail: "the spawn thread ended without a result (possible panic)".into(), entered_at: now }
        }
    }
}

/// `code N`, `signal N` or `unknown`: how a leg ended, for the diagnostic notes.
fn status_text(status: Option<ExitStatus>) -> String {
    match status {
        Some(ExitStatus::Code(c)) => format!("code {c}"),
        Some(ExitStatus::Signal(n)) => format!("signal {n}"),
        None => "unknown".into(),
    }
}

/// Today's accounting for a leg that ended with no storage cause: judge it
/// stable or not by its own recorded lifetime, count it (`storage::account`)
/// and respawn or go Terminal. `how` says how the leg ended, for the note.
#[allow(clippy::too_many_arguments)]
fn account_and_respawn(how: &str, consecutive_unstable_legs: &mut u32, storage_step: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &AuthorityState) -> Lifecycle {
    // stability is judged on the PRODUCER's own recorded lifetime
    // (`leg_was_stable`), never on a wall-clock interval measured from
    // Ready to THIS observation -- a slow capsule teardown (job reap,
    // ConPTY drain, the aggregate deadline, a final wait) could alone
    // exceed the stability interval with nothing to do with how long the
    // producer itself actually ran.
    let voyage_id = authority.voyage_id.clone().expect("a leg ended, so a voyage_id is Some");
    let unstable = !leg_was_stable(&config.state_dir, &voyage_id);
    storage::account(consecutive_unstable_legs, storage_step, unstable);
    note(format_args!(
        "leg ended {how} (unstable={unstable}) consecutive_unstable_legs={consecutive_unstable_legs}"
    ));
    respawn_or_terminal(consecutive_unstable_legs, capsule_exe, config, lease, authority, unstable)
}

/// What follows a leg's death with this exit `status`: a storage exit (71)
/// holds the authority and respawns when storage clears, an unknown status
/// is settled by one immediate durable probe, and anything else is today's
/// crash accounting.
#[allow(clippy::too_many_arguments)]
fn after_leg_death(status: Option<ExitStatus>, how: &str, consecutive_unstable_legs: &mut u32, storage_step: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &AuthorityState) -> Lifecycle {
    match storage::leg_death(status) {
        storage::LegDeath::Storage => {
            note(format_args!("leg ended {how} (storage exhausted); waiting for storage"));
            Lifecycle::StorageFull(storage::Wait::new(storage::Resume::Respawn))
        }
        storage::LegDeath::Unknown => {
            note(format_args!("leg ended {how}; probing the state root to settle whether storage is the cause"));
            Lifecycle::StorageFull(storage::Wait::new(storage::Resume::Suspect))
        }
        storage::LegDeath::Ordinary => {
            account_and_respawn(how, consecutive_unstable_legs, storage_step, capsule_exe, config, lease, authority)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn advance_ready(process: LegProcess, consecutive_unstable_legs: &mut u32, storage_step: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &AuthorityState, now: Instant) -> Lifecycle {
    match process.wait(Duration::ZERO) {
        Ok(true) => {
            // Single-owner reaping: `wait` just confirmed this leg's exit
            // and nothing below reads `process` again -- reap it now,
            // explicitly, here rather than relying on an implicit `Drop`
            // (see `ChallengedProcess::reap`'s own doc). An owned leg
            // answers with the exit status its one reap read; an adopted
            // leg's status is unknown. A Windows process HANDLE has no
            // zombie/reap concept -- `Drop`'s own `CloseHandle` is the
            // whole cleanup there.
            let status = process.reap();
            let how = format!("status={}", status_text(status));
            after_leg_death(status, &how, consecutive_unstable_legs, storage_step, capsule_exe, config, lease, authority)
        }
        Ok(false) => Lifecycle::Ready { process },
        Err(e) => Lifecycle::Terminal {
            detail: bounded_detail(format!("wait on the leg's process handle failed: {e}")),
            entered_at: now,
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn advance_ending(operation_id: String, rx: mpsc::Receiver<EndingProgress>, handle: JoinHandle<()>, started_at: Instant, mut pending_reply: Option<ConnId>, process: LegProcess, lane: &Lane, conns: &HashMap<ConnId, Conn>, consecutive_unstable_legs: &mut u32, storage_step: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &mut AuthorityState, now: Instant) -> Lifecycle {
    match rx.try_recv() {
        Ok(EndingProgress::RecordClosed) => {
            if let Some(conn_id) = pending_reply.take() {
                if conns.contains_key(&conn_id) {
                    let reply = SupervisorReply::Operation(SupervisorOperationState::RecordClosed);
                    let bytes = encode_reply_or_fallback(&reply);
                    let _ = lane.send(conn_id, bytes, None);
                } // else: client disconnected meanwhile -- fine (B3).
            }
            // Still in flight -- carry `process` forward untouched.
            Lifecycle::Ending { operation_id, rx, handle, started_at, pending_reply, process }
        }
        Ok(EndingProgress::Final(result)) => {
            join_and_warn(handle, "end_run");
            end_run_finished(result, &operation_id, process, consecutive_unstable_legs, storage_step, capsule_exe, config, lease, authority, now)
        }
        Err(mpsc::TryRecvError::Empty) => {
            if watchdog_expired(started_at, ENDING_WATCHDOG, now) {
                abandon_worker(handle, "end_run");
                retire_leg(&mut authority.retired_legs, process);
                Lifecycle::Terminal {
                    detail: format!("end_run {operation_id}: operation watchdog expired"),
                    entered_at: now,
                }
            } else {
                // Still in flight -- carry `process` forward untouched.
                Lifecycle::Ending { operation_id, rx, handle, started_at, pending_reply, process }
            }
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            join_and_warn(handle, "end_run");
            retire_leg(&mut authority.retired_legs, process);
            Lifecycle::Terminal {
                detail: format!("the end_run thread for {operation_id} ended without a result (possible panic)"),
                entered_at: now,
            }
        }
    }
}

/// What follows an `end_run` worker's final result. The retained `process`
/// is retired first in every case (reaped, or kept for the main loop's poll).
#[allow(clippy::too_many_arguments)]
fn end_run_finished(result: EndRunWorkerResult, operation_id: &str, process: LegProcess, consecutive_unstable_legs: &mut u32, storage_step: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &mut AuthorityState, now: Instant) -> Lifecycle {
    let status = retire_leg(&mut authority.retired_legs, process);
    match result {
        // The worker's own proven handle for this SAME leg is reaped inside
        // `finish_end_run_with_process` as always; THIS retained handle
        // needed its own retire/reap above, in case the leg exited on its own
        // (naturally, or via this end_run's own terminate) before the worker's
        // re-challenge ever observed it -- a second `waitid` on an
        // already-reaped pidfd is `ECHILD`, harmless. If it has NOT exited yet
        // (the worker's own re-challenge proved the writer gone by a marker,
        // not by watching this exact process die), `retire_leg` moved it into
        // `retired_legs` instead of dropping it.
        EndRunWorkerResult::Ended => Lifecycle::EndedNoRespawn,
        // The SAME routing a naturally-exited `Ready` leg gets (a storage
        // exit holds, an unknown status is probed, the rest is the
        // producer-recorded stability check): a pre-barrier failure still
        // means the writer is CONFIRMED gone (finish_end_run_with/
        // without_process already proved that before ever returning
        // PreBarrierFailed), so this leg's own producer_uptime_ms is equally
        // readable and authoritative here.
        EndRunWorkerResult::PreBarrierFailed => {
            let how = format!("status={} (end_run not durably accepted)", status_text(status));
            after_leg_death(status, &how, consecutive_unstable_legs, storage_step, capsule_exe, config, lease, authority)
        }
        EndRunWorkerResult::Storage(detail) => {
            note(format_args!("end_run {operation_id} met storage exhaustion ({detail}); waiting for storage"));
            Lifecycle::StorageFull(storage::Wait::new(storage::Resume::Recover))
        }
        EndRunWorkerResult::Fatal(detail) => {
            Lifecycle::Terminal { detail: format!("end_run {operation_id}: {detail}"), entered_at: now }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn advance_resetting(operation_id: String, rx: mpsc::Receiver<ResetWorkerResult>, handle: JoinHandle<()>, started_at: Instant, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &mut AuthorityState, now: Instant) -> Lifecycle {
    match rx.try_recv() {
        Ok(ResetWorkerResult::Done { new_voyage }) => {
            join_and_warn(handle, "reset");
            authority.voyage_id = Some(new_voyage.clone());
            // Spawn IMMEDIATELY for the new voyage, never an
            // adopt-only probe first: the freshly-minted voyage
            // is definitely empty.
            match lease.for_spawn() {
                Ok(spawn_lease) => {
                    let voyage_root = voyage_root_path(&config.state_dir, &new_voyage);
                    let (rx, handle) = spawn_owned_spawn_attempt(
                        capsule_exe.to_path_buf(),
                        voyage_root,
                        new_voyage,
                        config.cols,
                        config.rows,
                        spawn_lease,
                        config.survival,
                        config.producer_argv.clone(),
                    );
                    Lifecycle::Spawning { rx, handle, started_at: now }
                }
                Err(e) => Lifecycle::Terminal {
                    detail: bounded_detail(format!(
                        "could not prepare the parent-death lease for a fresh spawn: {e}"
                    )),
                    entered_at: now,
                },
            }
        }
        Ok(ResetWorkerResult::Storage(detail)) => {
            join_and_warn(handle, "reset");
            note(format_args!("reset {operation_id} met storage exhaustion ({detail}); waiting for storage"));
            Lifecycle::StorageFull(storage::Wait::new(storage::Resume::Recover))
        }
        Ok(ResetWorkerResult::Fatal(detail)) => {
            join_and_warn(handle, "reset");
            Lifecycle::Terminal { detail, entered_at: now } // B2
        }
        Err(mpsc::TryRecvError::Empty) => {
            if watchdog_expired(started_at, RESETTING_WATCHDOG, now) {
                abandon_worker(handle, "reset");
                Lifecycle::Terminal {
                    detail: format!("reset {operation_id}: operation watchdog expired"),
                    entered_at: now,
                }
            } else {
                Lifecycle::Resetting { operation_id, rx, handle, started_at }
            }
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            join_and_warn(handle, "reset");
            Lifecycle::Terminal {
                detail: format!("the reset thread for {operation_id} ended without a result (possible panic)"),
                entered_at: now,
            }
        }
    }
}

/// One tick of the storage wait (`storage::advance`): while it holds, nothing
/// else changes; a resume respawns the same voyage with its full argv or
/// re-runs startup recovery, and a suspected leg death that passed its probe
/// takes the ordinary accounting.
#[allow(clippy::too_many_arguments)]
pub(super) fn advance_storage_full(wait: storage::Wait, consecutive_unstable_legs: &mut u32, storage_step: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &AuthorityState, now: Instant) -> Lifecycle {
    match storage::advance(wait, storage_step, &authority.state_dir, now) {
        storage::Outcome::Waiting(wait) => Lifecycle::StorageFull(wait),
        storage::Outcome::Resume(storage::Resume::Respawn) => {
            note(format_args!("storage is back; respawning the leg"));
            respawn_or_terminal(consecutive_unstable_legs, capsule_exe, config, lease, authority, false)
        }
        storage::Outcome::Resume(storage::Resume::Recover) => {
            note(format_args!("storage is back; re-running startup recovery"));
            let (rx, handle) = spawn_recovery(config.state_dir.clone(), config.mode);
            Lifecycle::Recovering { rx, handle, started_at: now }
        }
        storage::Outcome::Resume(storage::Resume::Suspect) => {
            account_and_respawn("with no storage cause", consecutive_unstable_legs, storage_step, capsule_exe, config, lease, authority)
        }
        storage::Outcome::Terminal(detail) => Lifecycle::Terminal { detail, entered_at: now },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::probe::ProbeOps;

    struct Fixture {
        _dir: tempfile::TempDir,
        config: SuperviseConfig,
        lease: LegLease,
        authority: AuthorityState,
        capsule_exe: PathBuf,
    }

    /// The lease is a named mutex on Windows (named by the fixture and the process
    /// id), so tests running in parallel each need a name of their own.
    static NEXT_FIXTURE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().to_path_buf();
        let config = SuperviseConfig {
            state_dir: state_dir.clone(),
            mode: StartMode::Start,
            producer_argv: vec!["unused".into()],
            cols: 80,
            rows: 24,
            assume_no_rollback_target: true,
            survival: Survival::Normal,
            first_leg_without: Vec::new(),
        };
        let authority = AuthorityState {
            state_dir,
            voyage_id: Some(uuid::Uuid::now_v7().to_string()),
            self_pid: 0,
            self_created: 0,
            stop_requested: None,
            producer_ran: false,
            retired_legs: Vec::new(),
        };
        Fixture {
            _dir: dir,
            config,
            lease: LegLease::create(&format!("transitions-test-{}", NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::SeqCst))).unwrap(),
            authority,
            capsule_exe: PathBuf::from("unused"),
        }
    }

    /// An owned leg that has already exited, as the lifecycle holds one.
    fn exited_leg() -> LegProcess {
        #[cfg(unix)]
        let mut command = std::process::Command::new("/bin/sh");
        #[cfg(unix)]
        command.args(["-c", "exit 0"]);
        #[cfg(windows)]
        let mut command = std::process::Command::new("cmd.exe");
        #[cfg(windows)]
        command.args(["/d", "/c", "exit 0"]);
        let crate::supervisor::probe::SpawnOutcome::Spawned(child) = RealProbeOps.spawn(&mut command) else {
            panic!("spawn a short-lived leg");
        };
        LegProcess::Owned(child)
    }

    fn spawning(outcome: ProbeOutcome<LegProcess>) -> Lifecycle {
        let (tx, rx) = mpsc::channel();
        tx.send(outcome).unwrap();
        Lifecycle::Spawning { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now() }
    }

    fn advance_one_spawning(f: &mut Fixture, counter: &mut u32, outcome: ProbeOutcome<LegProcess>) -> Lifecycle {
        let Lifecycle::Spawning { rx, handle, started_at } = spawning(outcome) else { unreachable!() };
        advance_spawning(rx, handle, started_at, counter, &f.capsule_exe, &f.config, &f.lease, &mut f.authority, Instant::now())
    }

    /// A leg that exits 71 is a storage exit: the crash counter is left
    /// alone and the authority holds. The same leg with any other code is
    /// counted, and the third unstable leg is Terminal.
    #[test]
    fn a_storage_leg_exit_keeps_the_crash_counter() {
        let mut f = fixture();
        let mut counter = 2;
        let held = advance_one_spawning(&mut f, &mut counter, ProbeOutcome::LegEnded(Some(ExitStatus::Code(71))));
        assert!(!matches!(held, Lifecycle::Terminal { .. }), "a storage exit never goes Terminal");
        assert!(matches!(held, Lifecycle::StorageFull(_)), "a storage exit holds the authority");
        assert_eq!(counter, 2, "a storage exit leaves the counter alone");

        let mut counter = 2;
        let counted = advance_one_spawning(&mut f, &mut counter, ProbeOutcome::LegEnded(Some(ExitStatus::Code(1))));
        assert!(matches!(counted, Lifecycle::Terminal { .. }), "an ordinary exit is counted: the third is Terminal");
        assert_eq!(counter, 3);
    }

    /// A storage failure of any authority worker holds the authority and
    /// re-runs startup recovery once storage clears; nothing goes Terminal.
    #[test]
    fn authority_storage_failures_wait() {
        let mut f = fixture();
        let now = Instant::now();
        let expect = |lifecycle: Lifecycle, resume: storage::Resume| match lifecycle {
            Lifecycle::StorageFull(wait) => assert_eq!(wait.resume(), resume),
            _ => panic!("expected the storage wait"),
        };

        // Recovery.
        let (tx, rx) = mpsc::channel();
        tx.send(RecoveryOutcome::Storage("full".into())).unwrap();
        let next = advance_recovering(rx, std::thread::spawn(|| {}), now, &f.config, &mut f.authority, now);
        expect(next, storage::Resume::Recover);

        // An end_run worker.
        let (mut counter, mut step) = (0, 0);
        let next = end_run_finished(
            EndRunWorkerResult::Storage("full".into()),
            "op-1",
            exited_leg(),
            &mut counter,
            &mut step,
            &f.capsule_exe.clone(),
            &f.config,
            &f.lease,
            &mut f.authority,
            now,
        );
        expect(next, storage::Resume::Recover);
        assert_eq!(counter, 0, "an end_run storage failure does not touch the counter");

        // A reset worker.
        let (tx, rx) = mpsc::channel();
        tx.send(ResetWorkerResult::Storage("full".into())).unwrap();
        let next = advance_resetting("op-2".into(), rx, std::thread::spawn(|| {}), now, &f.capsule_exe.clone(), &f.config, &f.lease, &mut f.authority, now);
        expect(next, storage::Resume::Recover);
    }

    /// The first-leg tokens are stripped while no producer has run in this
    /// process and after an unstable leg, and kept after a stable one; a leg
    /// that reaches Ready is the producer having run.
    #[test]
    fn a_respawn_before_any_producer_ran_keeps_the_first_leg_tokens_stripped() {
        let mut f = fixture();
        f.config.producer_argv = vec!["agent".into(), "--continue".into()];
        f.config.first_leg_without = vec!["--continue".into()];
        let stripped = vec!["agent".to_string()];
        let whole = f.config.producer_argv.clone();
        assert_eq!(leg_argv(&f.config, false, false), stripped, "no producer ran: stripped");
        assert_eq!(leg_argv(&f.config, true, false), whole, "a producer ran and the leg was stable: kept");
        assert_eq!(leg_argv(&f.config, true, true), stripped, "an unstable leg: stripped");

        // A leg that reaches Ready sets `producer_ran`.
        assert!(!f.authority.producer_ran);
        let mut counter = 0;
        let ready = advance_one_spawning(&mut f, &mut counter, ProbeOutcome::Ready(exited_leg()));
        assert!(matches!(ready, Lifecycle::Ready { .. }));
        assert!(f.authority.producer_ran);
    }
}
