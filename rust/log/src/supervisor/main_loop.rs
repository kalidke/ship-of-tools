//! The main authority loop: `supervise_inner`, with its setup and exit pieces.

use super::*;

// ---------------------------------------------------------------------
// The main authority loop
// ---------------------------------------------------------------------

pub(super) fn supervise_inner(config: SuperviseConfig) -> crate::Result<i32> {
    init_process_globals(&config)?;

    std::fs::create_dir_all(voyages_dir(&config.state_dir))?;

    // ONE AUTHORITY.
    let _fence = match crate::fence::lock_supervisor(&config.state_dir) {
        Ok(f) => f,
        // `Error::State` is the ONE error `lock_supervisor` can return for
        // "already held" (see `EXIT_CONTENDED`'s own doc for why this is
        // the only path that produces it here) -- distinct from a genuine
        // bootstrap/IO failure (`Error::Io`), which stays EXIT_TERMINAL.
        Err(e @ crate::Error::State(_)) => {
            note(format_args!("authority fence already held by a live supervisor: {e}"));
            return Ok(EXIT_CONTENDED);
        }
        Err(e) => {
            note(format_args!("could not become the authority: {e}"));
            return Ok(EXIT_TERMINAL);
        }
    };

    let h = crate::state_dir::state_dir_hash(&config.state_dir);

    // The lane: bound AFTER the fence, BEFORE any adopt or spawn.
    let lane = match Lane::bind_supervisor(&h, MAX_LANE_INSTANCES) {
        Ok(l) => l,
        Err(e) => {
            note(format_args!("could not bind the supervisor lane: {e}"));
            return Ok(EXIT_TERMINAL);
        }
    };

    // The parent-death lease: created ONCE, held for this process's
    // whole life.
    let lease = match LegLease::create(&h) {
        Ok(l) => l,
        Err(e) => {
            note(format_args!("could not create the parent-death lease: {e}"));
            return Ok(EXIT_TERMINAL);
        }
    };

    let self_ids = self_pid_and_created().unwrap_or((0, 0));
    let mut authority = AuthorityState {
        state_dir: config.state_dir.clone(),
        voyage_id: None,
        self_pid: self_ids.0,
        self_created: self_ids.1,
        stop_requested: None,
        retired_legs: Vec::new(),
    };
    let mut conns: HashMap<ConnId, Conn> = HashMap::new();
    // The real, resolved path; [`build_run_command`] swaps the ACTUAL
    // exec target for `/proc/self/exe` on Linux (see its own comment).
    let capsule_exe = std::env::current_exe().map_err(crate::Error::Io)?;

    // B1: recovery + pointer discovery, folded into ONE non-blocking
    // background worker — the lane is already up and serviced from the
    // very first loop iteration below, well before either concludes.
    let (rx, handle) = spawn_recovery(config.state_dir.clone(), config.mode);
    let mut lifecycle = Lifecycle::Recovering { rx, handle, started_at: Instant::now() };

    let mut consecutive_unstable_legs: u32 = 0;

    // Switch-latency Phase 1: the event this loop's own tail wait
    // ([`Lane::events`]'s `recv_timeout`, replacing an unconditional
    // `sleep(MAIN_LOOP_POLL)`) woke on, carried forward as the very first
    // thing the NEXT `service_lane` call processes — see that call site's
    // own comment for why a plain `try_recv` there would otherwise miss
    // it (a channel `recv_timeout` already consumes the item it returns).
    let mut woke_on: Option<LaneEvent> = None;

    'authority: loop {
        let now = Instant::now();
        {
            let mut lane_ctx = LaneCtx { authority: &mut authority, lifecycle: &mut lifecycle };
            if service_lane(&lane, woke_on.take(), &mut conns, &mut lane_ctx, now) {
                force_terminal(
                    &mut lifecycle,
                    &mut authority.retired_legs,
                    "supervisor lane accept loop failed permanently".into(),
                );
            }
        }

        let current = std::mem::replace(
            &mut lifecycle,
            Lifecycle::Terminal { detail: "transitioning".into(), entered_at: now },
        );
        lifecycle = match current {
            Lifecycle::Recovering { rx, handle, started_at } => match rx.try_recv() {
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
            },
            Lifecycle::InitialProbe { rx, handle, started_at } => match rx.try_recv() {
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
                                    capsule_exe.clone(),
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
            },
            Lifecycle::Spawning { rx, handle, started_at } => match rx.try_recv() {
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
                    consecutive_unstable_legs += 1;
                    note(format_args!(
                        "leg failed to spawn: {e} (unstable=true) consecutive_unstable_legs={consecutive_unstable_legs}"
                    ));
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, true)
                }
                Ok(ProbeOutcome::KilledAfterTimeout | ProbeOutcome::LegEnded) => {
                    join_and_warn(handle, "spawn");
                    consecutive_unstable_legs += 1;
                    note(format_args!(
                        "leg ended before reaching Ready (unstable=true) consecutive_unstable_legs={consecutive_unstable_legs}"
                    ));
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, true)
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
            },
            Lifecycle::Ready { process } => match process.wait(Duration::ZERO) {
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
                        consecutive_unstable_legs += 1;
                    } else {
                        consecutive_unstable_legs = 0;
                    }
                    note(format_args!(
                        "leg ended (unstable={unstable}) consecutive_unstable_legs={consecutive_unstable_legs}"
                    ));
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, unstable)
                }
                Ok(false) => Lifecycle::Ready { process },
                Err(e) => Lifecycle::Terminal {
                    detail: bounded_detail(format!("wait on the leg's process handle failed: {e}")),
                    entered_at: now,
                },
            },
            Lifecycle::Ending { operation_id, rx, handle, started_at, mut pending_reply, process } => match rx.try_recv() {
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
                        consecutive_unstable_legs += 1;
                    } else {
                        consecutive_unstable_legs = 0;
                    }
                    note(format_args!(
                        "leg ended (end_run not durably accepted; unstable={unstable}) consecutive_unstable_legs={consecutive_unstable_legs}"
                    ));
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, unstable)
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
            },
            Lifecycle::Resetting { operation_id, rx, handle, started_at } => match rx.try_recv() {
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
                                capsule_exe.clone(),
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
            },
            other @ (Lifecycle::EndedNoRespawn | Lifecycle::Terminal { .. }) => other,
        };

        // G1 (Codex review round 2): a leg retired while still alive
        // (`retire_leg`, above) has no other owner watching it — poll it
        // here, once per tick, the SAME `MAIN_LOOP_POLL` cadence every
        // other wait in this loop already uses (no new timer). Runs
        // regardless of `exit_now` below: a leg's own death is exactly as
        // worth observing (and reaping, Linux) on the loop's very last
        // iteration as any other.
        reap_retired_legs(&mut authority.retired_legs);

        let exit_now = should_exit_now(&lifecycle, &authority, &conns, now);
        if exit_now {
            break 'authority;
        }

        // Switch-latency Phase 1: wake the instant the lane has something
        // (a connection accepted, or a frame readable — anything
        // `LaneServer::events()` can produce) instead of always paying the
        // full idle cadence before the NEXT `service_lane` call even looks.
        // `MAIN_LOOP_POLL` is unchanged as the IDLE bound: with nothing on
        // the lane, this blocks the exact same 100 ms `sleep` used to —
        // zero added wakeups, zero added idle power. Only genuine lane
        // traffic makes this return sooner, which is the entire point (an
        // attach's status probe no longer waits out a tick it happens to
        // land inside). Never a second, uncoordinated wait alongside this
        // one: it IS this iteration's one wait, replacing the sleep in
        // place.
        woke_on = lane.events().recv_timeout(MAIN_LOOP_POLL).ok();
    }

    let exit_code = final_exit_code(&lifecycle, &authority);

    // `lane`'s own `Drop` performs the teardown when it goes out of
    // scope below.
    Ok(exit_code)
}

fn init_process_globals(config: &SuperviseConfig) -> crate::Result<()> {
    // ADR 0043 decision 25: set before ANYTHING else in this function —
    // every diagnostic below, including the SIGCHLD-reset failure two
    // lines down, goes through `note`, which reads this.
    let _ = NOTE_PREFIX.set(format!(
        "sot-capsule supervise[{}]",
        config.state_dir.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
    ));
    // ADR 0043 decision 21 (Codex review round, F2): SIGCHLD is SET to
    // SIG_DFL here, as the very first thing this function does -- never
    // merely assumed. Whatever launched this process (a daemon, a shell)
    // may have inherited `SIG_IGN` across `exec`, which auto-reaps every
    // child immediately and silently breaks the pid pin every Unix leg
    // identity is built on: on Linux a reaped pid can be recycled before
    // `SpawnedChild::from_child`'s own `pidfd_open` ever runs
    // (reproduced), and on macOS, which pins a leg by (pid, start time)
    // instead, a pid that vanishes before it is read is the same hole by
    // another road. Unix-wide for that reason, not Linux-specific: this
    // process OWNS the disposition it depends on, the same line
    // `producer_pty`'s own `spawn` already draws for its forked child.
    #[cfg(unix)]
    {
        if unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(crate::Error::Io(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

fn should_exit_now(lifecycle: &Lifecycle, authority: &AuthorityState, conns: &HashMap<ConnId, Conn>, now: Instant) -> bool {
    // N4 (Codex review round 3): `stop`'s own exit condition — the
    // underlying Lifecycle is NEVER touched by Stop's acceptance
    // (see `AuthorityState::stop_requested`'s own doc), so it keeps
    // resolving itself through EVERY ordinary transition arm above,
    // worker ownership/panic-detection/watchdogs all UNCHANGED and
    // fully shared with the non-stopping path. This only decides
    // WHEN to actually break the loop once accepted:
    //   - `Terminal` with NO stop pending: the pre-existing
    //     `TERMINAL_EXIT_GRACE` timer (now the `Lifecycle::Terminal`
    //     variant's own `entered_at` field — folded in, no separate
    //     `terminal_since` local needed).
    //   - `Terminal`, or a RESTING state (`Ready`/`EndedNoRespawn`),
    //     WITH a stop pending: "stop ends it sooner" — exit as soon
    //     as the primary connection's own reply has been delivered
    //     or given up on, via the SAME per-connection `PendingClose`
    //     gate every other reply already uses (bounded by
    //     `REFUSAL_SENT_DEADLINE` + `REFUSAL_FLUSH_GRACE`,
    //     ~2.25s — `service_lane` removes a connection from `conns`
    //     once that gate closes it, which is exactly the signal
    //     this reads).
    //   - Any OTHER state (a worker still genuinely in flight): never
    //     exits here regardless of a pending stop — "keep servicing
    //     its result until it lands or its own watchdog fires".
    match (&lifecycle, &authority.stop_requested) {
        (Lifecycle::Terminal { .. }, Some(stop)) => !conns.contains_key(&stop.primary_conn),
        (Lifecycle::Terminal { entered_at, .. }, None) => {
            now.saturating_duration_since(*entered_at) >= TERMINAL_EXIT_GRACE
        }
        (Lifecycle::Ready { .. } | Lifecycle::EndedNoRespawn, Some(stop)) => !conns.contains_key(&stop.primary_conn),
        _ => false,
    }
}

fn final_exit_code(lifecycle: &Lifecycle, authority: &AuthorityState) -> i32 {
    match (&lifecycle, &authority.stop_requested) {
        (Lifecycle::Terminal { detail, .. }, _) => {
            note(format_args!("exiting terminal: {detail}"));
            EXIT_TERMINAL
        }
        (_, Some(stop)) if stop.terminal_severity => {
            note(format_args!(
                "exiting terminal (a stop was accepted while already terminal, or its own journal \
                 write failed)"
            ));
            EXIT_TERMINAL
        }
        _ => EXIT_CLEAN,
    }
}
