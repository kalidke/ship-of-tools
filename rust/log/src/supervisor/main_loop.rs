//! The main authority loop: `supervise_inner`, with its setup and exit pieces.

use super::*;

// ---------------------------------------------------------------------
// The main authority loop
// ---------------------------------------------------------------------

pub(super) fn supervise_inner(config: SuperviseConfig) -> crate::Result<i32> {
    init_process_globals(&config)?;

    std::fs::create_dir_all(voyages_dir(&config.state_dir))?;

    // ONE AUTHORITY.
    let _fence = match crate::supervisor::journal::fence::lock_supervisor(&config.state_dir) {
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

    let h = crate::host::state_dir::state_dir_hash(&config.state_dir);

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

    // recovery + pointer discovery, folded into ONE non-blocking
    // background worker — the lane is already up and serviced from the
    // very first loop iteration below, well before either concludes.
    let (rx, handle) = spawn_recovery(config.state_dir.clone(), config.mode);
    let mut lifecycle = Lifecycle::Recovering { rx, handle, started_at: Instant::now(), first_leg: true };

    let mut consecutive_unstable_legs: u32 = 0;
    // The storage wait's backoff step: carried across a storage exit that
    // follows a successful probe, and back to 0 when a leg is judged stable.
    let mut storage_step: u32 = 0;

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
            Lifecycle::Recovering { rx, handle, started_at, first_leg } => advance_recovering(rx, handle, started_at, first_leg, &config, &mut authority, now),
            Lifecycle::InitialProbe { rx, handle, started_at, first_leg } => advance_initial_probe(rx, handle, started_at, first_leg, &capsule_exe, &config, &lease, &authority, now),
            Lifecycle::Spawning { rx, handle, started_at } => advance_spawning(rx, handle, started_at, &mut consecutive_unstable_legs, &capsule_exe, &config, &lease, &authority, now),
            Lifecycle::Ready { process } => advance_ready(process, &mut consecutive_unstable_legs, &mut storage_step, &capsule_exe, &config, &lease, &authority, now),
            Lifecycle::Ending { operation_id, rx, handle, started_at, pending_reply, process } => advance_ending(operation_id, rx, handle, started_at, pending_reply, process, &lane, &conns, &mut consecutive_unstable_legs, &mut storage_step, &capsule_exe, &config, &lease, &mut authority, now),
            Lifecycle::StorageFull(wait) => advance_storage_full(wait, &mut consecutive_unstable_legs, &mut storage_step, &capsule_exe, &config, &lease, &authority, now),
            Lifecycle::Resetting { operation_id, rx, handle, started_at } => advance_resetting(operation_id, rx, handle, started_at, &capsule_exe, &config, &lease, &mut authority, now),
            other @ (Lifecycle::EndedNoRespawn | Lifecycle::Terminal { .. }) => other,
        };

        // a leg retired while still alive
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
    // ADR 0043 decision 21: SIGCHLD is SET to
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
    // `capsule::producer::pty`'s own `spawn` already draws for its forked child.
    #[cfg(unix)]
    {
        if unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(crate::Error::Io(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

fn should_exit_now(lifecycle: &Lifecycle, authority: &AuthorityState, conns: &HashMap<ConnId, Conn>, now: Instant) -> bool {
    // `stop`'s own exit condition — the
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
        (Lifecycle::Ready { .. } | Lifecycle::EndedNoRespawn | Lifecycle::StorageFull(_), Some(stop)) => !conns.contains_key(&stop.primary_conn),
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

