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
pub(super) fn advance_initial_probe(rx: mpsc::Receiver<ProbeOutcome<Process>>, handle: JoinHandle<()>, started_at: Instant, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &AuthorityState, now: Instant) -> Lifecycle {
    match rx.try_recv() {
        Ok(ProbeOutcome::Adopted(process)) => {
            join_and_warn(handle, "initial probe");
            Lifecycle::Ready { process }
        }
        Ok(ProbeOutcome::Absent) => {
            join_and_warn(handle, "initial probe");
            let voyage_id = authority.voyage_id.clone().expect("set once Recovering completes");
            match should_spawn_after_absent(&config.state_dir, &voyage_id, config.mode) {
                Ok(true) => match lease.for_spawn() {
                    Ok(spawn_lease) => {
                        let voyage_root = voyage_root_path(&config.state_dir, &voyage_id);
                        // The very first leg THIS PROCESS spawns --
                        // see `strip_first_leg_tokens`'s own doc.
                        let argv = strip_first_leg_tokens(&config.producer_argv, &config.first_leg_without);
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
pub(super) fn advance_spawning(rx: mpsc::Receiver<ProbeOutcome<Process>>, handle: JoinHandle<()>, started_at: Instant, consecutive_unstable_legs: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &AuthorityState, now: Instant) -> Lifecycle {
    match rx.try_recv() {
        Ok(ProbeOutcome::Ready(process)) => {
            // No anti-flap accounting here at all — the counter
            // resets or increments ONLY once this leg's own
            // eventual death is observed and its recorded
            // producer_uptime_ms is read (`leg_was_stable`, N1),
            // never on merely reaching Ready. An earlier version
            // zeroed the counter HERE, before any death could
            // ever be counted against a prior unstable run: a
            // leg that died moments after every respawn was
            // "the first" unstable leg forever — 90 legs in
            // ~120s of real Windows CI, the anti-flap bound
            // never tripping.
            join_and_warn(handle, "spawn");
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
        Ok(ProbeOutcome::KilledAfterTimeout | ProbeOutcome::LegEnded) => {
            join_and_warn(handle, "spawn");
            *consecutive_unstable_legs += 1;
            note(format_args!(
                "leg ended before reaching Ready (unstable=true) consecutive_unstable_legs={consecutive_unstable_legs}"
            ));
            respawn_or_terminal(consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, true)
        }
        Ok(ProbeOutcome::Foreign) => {
            // Codex review round 2, finding M8: identity-
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

pub(super) fn advance_ready(process: Process, consecutive_unstable_legs: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &AuthorityState, now: Instant) -> Lifecycle {
    match process.wait(Duration::ZERO) {
        Ok(true) => {
            // Single-owner reaping (review round), every Unix:
            // `wait` just confirmed this leg's exit and nothing
            // below reads `process` again — reap it now,
            // explicitly, here rather than relying on an implicit
            // `Drop` (see `ChallengedProcess::reap`'s own doc).
            // This is the natural-exit transition F1's own doc
            // used to describe. A Windows process HANDLE has no
            // zombie/reap concept — `Drop`'s own `CloseHandle` is
            // the whole cleanup there.
            //
            // `cfg(unix)`, not `linux` — see
            // `finish_end_run_with_process`'s own reap comment.
            // This is the site a long-running macOS supervisor
            // would have leaked from hardest: one zombie per
            // natural leg exit, forever, because `SIGCHLD` is
            // `SIG_DFL` and nothing else auto-reaps.
            #[cfg(unix)]
            process.reap();
            // N1 (Codex review round 3): stability is judged on
            // the PRODUCER's own recorded lifetime
            // (`leg_was_stable`), never on a wall-clock interval
            // measured from Ready to THIS observation — a slow
            // capsule teardown (job reap, ConPTY drain, the
            // aggregate deadline, a final wait) could alone
            // exceed the stability interval with nothing to do
            // with how long the producer itself actually ran.
            let voyage_id = authority.voyage_id.clone().expect("Ready implies a voyage_id");
            let unstable = !leg_was_stable(&config.state_dir, &voyage_id);
            if unstable {
                *consecutive_unstable_legs += 1;
            } else {
                *consecutive_unstable_legs = 0;
            }
            note(format_args!(
                "leg ended (unstable={unstable}) consecutive_unstable_legs={consecutive_unstable_legs}"
            ));
            respawn_or_terminal(consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, unstable)
        }
        Ok(false) => Lifecycle::Ready { process },
        Err(e) => Lifecycle::Terminal {
            detail: bounded_detail(format!("wait on the leg's process handle failed: {e}")),
            entered_at: now,
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn advance_ending(operation_id: String, rx: mpsc::Receiver<EndingProgress>, handle: JoinHandle<()>, started_at: Instant, mut pending_reply: Option<ConnId>, process: Process, lane: &Lane, conns: &HashMap<ConnId, Conn>, consecutive_unstable_legs: &mut u32, capsule_exe: &Path, config: &SuperviseConfig, lease: &LegLease, authority: &mut AuthorityState, now: Instant) -> Lifecycle {
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
        Ok(EndingProgress::Final(EndRunWorkerResult::Ended)) => {
            join_and_warn(handle, "end_run");
            // Review round, reproduced; widened round 2 (G1): the
            // worker's own proven handle for this SAME leg is
            // reaped inside `finish_end_run_with_process` as
            // always; THIS retained handle needs its own
            // retire/reap here too, in case the leg exited on its
            // own (naturally, or via this end_run's own
            // terminate) before the worker's re-challenge ever
            // observed it -- a second `waitid` on an
            // already-reaped pidfd is `ECHILD`, harmless. If it
            // has NOT exited yet (the worker's own re-challenge
            // proved the writer gone by a marker, not by watching
            // this exact process die), `retire_leg` moves it into
            // `retired_legs` instead of dropping it here.
            retire_leg(&mut authority.retired_legs, process);
            Lifecycle::EndedNoRespawn
        }
        Ok(EndingProgress::Final(EndRunWorkerResult::PreBarrierFailed)) => {
            join_and_warn(handle, "end_run");
            retire_leg(&mut authority.retired_legs, process);
            // N1 (Codex review round 3): the SAME producer-
            // recorded stability check the natural-death Ready
            // arm uses — a pre-barrier failure still means the
            // writer is CONFIRMED gone (finish_end_run_with/
            // without_process already proved that before ever
            // returning PreBarrierFailed), so this leg's own
            // producer_uptime_ms is equally readable and
            // authoritative here.
            let voyage_id = authority.voyage_id.clone().expect("Ending implies a voyage_id");
            let unstable = !leg_was_stable(&config.state_dir, &voyage_id);
            if unstable {
                *consecutive_unstable_legs += 1;
            } else {
                *consecutive_unstable_legs = 0;
            }
            note(format_args!(
                "leg ended (end_run not durably accepted; unstable={unstable}) consecutive_unstable_legs={consecutive_unstable_legs}"
            ));
            respawn_or_terminal(consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, unstable)
        }
        Ok(EndingProgress::Final(EndRunWorkerResult::Fatal(detail))) => {
            join_and_warn(handle, "end_run");
            retire_leg(&mut authority.retired_legs, process);
            Lifecycle::Terminal { detail: format!("end_run {operation_id}: {detail}"), entered_at: now }
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
