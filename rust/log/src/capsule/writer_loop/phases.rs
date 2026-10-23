//! The writer loop after `start`, in `run`'s order: the main loop, Phase A (reap), Phase B (drain), the worker join and the run end.
use super::output_path::pace_output;
use super::row_requests::service_row;
use super::*;

pub(super) fn main_loop<P: Producer>(commands: &mpsc::Receiver<Command>, mut leg: Leg<P>) -> Result<(Leg<P>, ExitKind)> {
    // Main loop: natural-exit polled every iteration (bounded to one LOOP_TICK of latency, regardless of event volume);
    // the caller's command channel is polled NON-BLOCKINGLY (rare traffic, and this is the last point it is EVER polled:
    // teardown never touches it again, which is what makes admission revocation real). The row socket's requests are
    // served every iteration too.

    let exit_kind = 'main: loop {
        if leg.producer.wait(Duration::ZERO)? {
            break 'main ExitKind::ProducerExited;
        }
        service_row(&mut leg);
        match commands.try_recv() {
            // `Command::Kill` is the supervisor's EndRun primitive (this module's own doc on `Command`): it carries a
            // reason and latches the end durably in `ended`, or a resume could respawn a run that a caller
            // deliberately ended.
            Ok(Command::Kill) => {
                leg.shutdown_reason.get_or_insert_with(|| "operator_kill".to_string());
                latch_run_end(&mut leg, "operator_kill".to_string())?;
                break 'main ExitKind::Requested;
            }
            Ok(Command::Stop) => {
                leg.shutdown_reason.get_or_insert_with(|| "supervisor_stop".to_string());
                break 'main ExitKind::Requested;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            // The caller dropped its `Sender` — NOT a kill (ADR: no
            // channel-disconnect-as-kill, "no exit code, no FE event, no
            // supervisor inference may request one"). Just means no
            // FUTURE commands will arrive; keep running on natural-exit
            // polling alone. `try_recv` on an already-disconnected channel
            // returns immediately, so there is no cost to leaving this
            // arm empty rather than tracking "stop trying".
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        // A request on the row socket arriving DURING this wait wakes it through `ReaderEvent::Wake`, the instant the
        // row queues it. `LOOP_TICK` stays the bound on how long output may batch under sustained load and the cadence
        // when nothing is pending; with output pending, `OUTPUT_IDLE` of quiet commits it.
        match leg.output_rx.recv_timeout(LOOP_TICK) {
            Ok(ReaderEvent::Output(bytes)) => {
                pace_output(bytes, &mut leg)?;
            }
            // Nothing to do: `service_row` at this loop's own top (next iteration) serves whatever prompted this wake.
            Ok(ReaderEvent::Wake) => {}
            Ok(ReaderEvent::Done(result)) => {
                // The output side ended before this loop closed it, whether
                // by a graceful EOF or a real error. Fatal UNLESS the
                // producer's own exit explains it (ADR 0043 decision 12, as
                // amended): on macOS the session leader's exit revokes every
                // fd on the pty, this loop's deliberately held slave
                // included, so the reader's terminal state can PRECEDE the
                // exit being observable. Confirm it, bounded; never assume
                // it. Unconfirmed, this stays the anomaly it always was --
                // ConPTY keeps `hOutput` open regardless of child lifetime
                // until explicitly closed, and Linux's held slave means the
                // master sees nothing before the loop drops it, so on those
                // platforms only a capsule-runtime defect gets here -- and it
                // bails unsealed, matching ADR 0039's crash shape: recovery
                // seals whatever valid prefix already committed.
                if !leg.producer.wait(READER_END_EXIT_GRACE)? {
                    return Err(Error::State(format!(
                        "capsule_win: reader reached its terminal state before close_pty was ever \
                         called, and the producer was still alive {READER_END_EXIT_GRACE:?} later: {result:?}"
                    )));
                }
                leg.output_ended_early = Some(format!("{result:?}"));
                break 'main ExitKind::ProducerExited;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(ReaderEvent::ReaderGone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                // `ReaderGone` (an explicit send,
                // including from a panic unwind — see its own doc) and a
                // bare channel disconnect are the SAME condition now — the
                // reader thread is gone without having sent a terminal
                // `Done` — so they share this one arm rather than treating
                // an unwind as a distinct, undiagnosed case.
                return Err(Error::State(
                    "capsule_win: the reader thread ended without a terminal Done event".into(),
                ));
            }
        }
        // Checked here, after the match rather than only inside its `Timeout` arm, so a row wake (or any other
        // non-`Timeout` result) can never starve the screen file's pace.
        if let Some(screen) = &mut leg.screen {
            screen.tick(&leg.parser, Instant::now());
        }
    };
    Ok((leg, exit_kind))
}

pub(super) fn reap_domain<P: Producer>(mut leg: Leg<P>) -> Result<Leg<P>> {
    // ONE teardown orchestrator (ADR 0041: "Teardown has ONE orchestrator")
    // for both exit_kind::ProducerExited and exit_kind::Requested — every
    // step below is unconditional: terminating an already-empty job is a
    // harmless no-op. `commands` is never read again from this point on —
    // real admission revocation (module doc), not receive-then-discard.
    //
    // Phase A: terminate the job, then REAP-POLL `ActiveProcesses` WHILE
    // STILL SERVICING `output_rx` (committing frames, answering the
    // handshake).
    leg.producer.terminate_domain()?;
    let reap_deadline = Instant::now() + TEARDOWN_REAP_TIMEOUT;
    loop {
        if leg.producer.domain_is_empty()? {
            break;
        }
        if Instant::now() >= reap_deadline {
            return Err(Error::State(
                "capsule_win: job did not reap within the teardown timeout".into(),
            ));
        }
        match leg.output_rx.recv_timeout(TEARDOWN_REAP_POLL) {
            Ok(ReaderEvent::Output(bytes)) => {
                pace_output(bytes, &mut leg)?;
            }
            Ok(ReaderEvent::Wake) => {}
            Ok(ReaderEvent::Done(result)) => {
                // The same rule as the main loop's identical arm, and for
                // the same reason: nothing has called close_pty() yet, so
                // this cannot be an ordinary end of the drain -- it is the
                // producer's own exit revoking the pty, or it is a defect.
                // The difference here is only that this loop has a reap to
                // finish, so it records and keeps polling.
                if !leg.producer.wait(READER_END_EXIT_GRACE)? {
                    return Err(Error::State(format!(
                        "capsule_win: reader reached its terminal state during reap with the producer \
                         still alive {READER_END_EXIT_GRACE:?} later: {result:?}"
                    )));
                }
                leg.output_ended_early = Some(format!("{result:?}"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {} // just recheck active_processes
            Ok(ReaderEvent::ReaderGone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                // see the main loop's identical arm
                // — `ReaderGone` and a bare disconnect are the same "reader
                // thread is gone without a terminal Done" condition. The one
                // exception: after a terminal `Done` this teardown already
                // accounted for (above, or in the main loop), the reader's
                // own drop-guard `ReaderGone` is that event's EXPECTED
                // trailer, not a second anomaly.
                if leg.output_ended_early.is_none() {
                    return Err(Error::State(
                        "capsule_win: the reader thread ended without a terminal Done event during reap".into(),
                    ));
                }
            }
        }
    }
    Ok(leg)
}

pub(super) fn drain_output<P: Producer>(mut leg: Leg<P>) -> Result<(Leg<P>, std::thread::JoinHandle<()>)> {
    // Phase B: close the pseudoconsole on a DEDICATED thread so THIS loop
    // can keep draining `output_rx` (feeding `handle_output`, answering
    // the handshake) CONCURRENTLY with the close — the documented call
    // pattern ("reader already draining, THEN call this") applied
    // literally: draining must never itself pause to make the call. Both a
    // graceful EOF and a broken-pipe error are the ORDINARY, expected end
    // of this drain (the close is what produces them) — unlike Phase A's
    // identical-looking check, neither is an anomaly here.
    let closer_handle = leg.producer.close_output_side();
    // The close itself is UNCONDITIONAL -- it drops the held slave (a
    // real close(2), still owed on a revoked fd), keeps `Drop`
    // idempotent, and yields the `closer_handle` the aggregate join
    // below needs. Only the DRAIN is guarded: when the output side
    // already reached its terminal state before this point (see the
    // arms above), there is no EOF left for this loop to wait out, and
    // waiting for one would burn `TEARDOWN_DRAIN_TIMEOUT` and then fail
    // a run that is in fact complete.
    if leg.output_ended_early.is_none() {
        let drain_deadline = Instant::now() + TEARDOWN_DRAIN_TIMEOUT;
        loop {
            // checked here, unconditionally, every
            // iteration — mirroring Phase A's `reap_deadline` just above and
            // the main loop's own commit-deadline fix — rather than only
            // inside the `Timeout` arm below, where continuous row activity
            // activity could starve it exactly as it did the commit deadline.
            if Instant::now() >= drain_deadline {
                return Err(Error::State(
                    "capsule_win: reader did not reach EOF within the teardown drain timeout".into(),
                ));
            }
            match leg.output_rx.recv_timeout(TEARDOWN_DRAIN_POLL) {
                Ok(ReaderEvent::Output(bytes)) => {
                    pace_output(bytes, &mut leg)?;
                }
                Ok(ReaderEvent::Wake) => {}
                Ok(ReaderEvent::Done(_)) => {
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(ReaderEvent::ReaderGone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // see the main loop's identical arm.
                    return Err(Error::State(
                        "capsule_win: the reader thread ended without a terminal Done event during drain".into(),
                    ));
                }
            }
        }
    }
    Ok((leg, closer_handle))
}

pub(super) fn join_workers<P: Producer>(
    closer_handle: std::thread::JoinHandle<()>,
    reader_handle: std::thread::JoinHandle<()>,
    leg: &mut Leg<P>,
) -> Result<()> {
    // ONE absolute aggregate deadline, shared by this module's closer and reader threads. Cancellation for them already
    // happened earlier (Phase A's `producer.terminate_domain()`, Phase B's `producer.close_output_side()` on
    // `closer_handle`), so both are expected to be at or near their own natural return, and `join_within` (never the raw
    // blocking `.join()`) bounds the residual gap between "signalled EOF/exit" and "the thread function returned".
    // Expiry is TERMINAL: `run` must not report success, nor let the supervisor go on to a next run, past a
    // teardown that could not prove every worker stopped.
    let teardown_deadline = Instant::now() + TEARDOWN_AGGREGATE_DEADLINE;
    // The terminal's input thread: what waits is answered `run ended`, and a write blocked in the hung-up terminal returns.
    if !leg.input.retire(Duration::from_secs(2)) {
        leg_note(format_args!("sot-capsule: the terminal's input thread did not end within its bound"));
    }
    let closer_ok = join_within(closer_handle, teardown_deadline);
    let reader_ok = join_within(reader_handle, teardown_deadline);
    if !(closer_ok && reader_ok) {
        return Err(Error::State(format!(
            "capsule_win: aggregate teardown did not complete within its {TEARDOWN_AGGREGATE_DEADLINE:?} \
             deadline (closer ok={closer_ok}, reader ok={reader_ok}); \
             refusing to report success past an unproven teardown"
        )));
    }
    Ok(())
}

pub(super) fn finish_run<P: Producer>(exit_kind: ExitKind, producer_uptime_ms: u64, mut leg: Leg<P>) -> Result<ExitSummary> {
    // The producer's own exit status, raw and unsigned end to end for the Windows `Code` case (a Unix-style `i32` cast
    // would turn a high-bit NTSTATUS-shaped code negative for no reason). `wait()` first establishes the honesty-bound
    // precondition `exit_status_after_confirmed_exit`'s own doc requires: `domain_is_empty` above already proved the
    // process isn't running, but this satisfies the bound by the letter of its doc, not just by inference.
    if !leg.producer.wait(Duration::from_secs(5))? {
        return Err(Error::State(
            "capsule_win: producer did not signal after its domain reaped to empty".into(),
        ));
    }
    let exit_status = leg.producer.exit_status_after_confirmed_exit()?;

    // The last screen, at the agent's end; a failed write is noted, never fatal.
    if let Some(screen) = &mut leg.screen {
        screen.finish(&leg.parser, Instant::now(), SCREEN_FINISH_BOUND);
    }

    // What the supervisor reads: whether an end was asked for (`shutdown_reason` is the end's reason), how long the producer lived (`producer_uptime_ms` was captured when the main loop concluded, NOT here,
    // past all the teardown machinery this point sits after), and how it exited. A write that fails ends the leg
    // with an error and no file: the supervisor then reads the leg as crashed and unstable, which is what it was.
    let mut ended = Ended {
        finished: true,
        requested: leg.run_end_latched,
        uptime_ms: Some(producer_uptime_ms),
        reason: leg.shutdown_reason.clone(),
        output_ended_early: leg.output_ended_early.clone(),
        ..Ended::default()
    };
    match exit_status {
        ExitStatus::Code(c) => ended.exit_code = Some(c),
        ExitStatus::Signal(n) => ended.signal = Some(n),
    }
    ended::write(&leg.state_dir, &ended)?;

    Ok(ExitSummary {
        exit_code: Some(exit_status),
        exit_kind,
        handshake_answered: leg.dsr_answered,
        handshake_suppressed_matches: leg.handshake_suppressed_matches,
        resize_os_calls: leg.resize_os_calls,
    })
}

/// An end was asked for: say so in `ended`, durably, before anything acknowledges it, and latch. Idempotent past the
/// first: the first request wins. Used by the caller's `Kill`. A write that fails propagates with the latch unset, so
/// nothing was promised.
pub(super) fn latch_run_end<P: Producer>(leg: &mut Leg<P>, reason: String) -> Result<()> {
    if !leg.run_end_latched {
        ended::write(&leg.state_dir, &Ended { requested: true, reason: Some(reason), ..Ended::default() })?;
        leg.run_end_latched = true;
    }
    Ok(())
}
