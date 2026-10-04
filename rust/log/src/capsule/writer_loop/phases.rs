//! The writer loop after `start`, in `run`'s order: the main loop, Phase A (reap), Phase B (drain), the shutdown-ack grace, the worker join and the run end.
use super::lanes::{
    drain_pending_sends_only, execute_actions, execute_teardown_actions, service_transport_events,
    service_transport_events_teardown,
};
use super::output_path::{eager_ground_check, flush_output, maybe_rotate, pace_output};
use super::*;

pub(super) fn main_loop<'t, P: Producer>(commands: &mpsc::Receiver<Command>, mut leg: Leg<'t, P>) -> Result<(Leg<'t, P>, ExitKind)> {
    // Main loop: natural-exit polled every iteration (bounded to one
    // GROUP_COMMIT_WINDOW of latency, regardless of event volume); the
    // caller's command channel is polled NON-BLOCKINGLY (rare traffic, and
    // this is the last point it is EVER polled — teardown never touches it
    // again, which is what makes admission revocation real). The wire
    // transport is serviced every iteration too (`service_transport_events`
    // + `tick`) — see the module doc's "Step 5 (U2)" section.

    let exit_kind = 'main: loop {
        if leg.producer.wait(Duration::ZERO)? {
            break 'main ExitKind::ProducerExited;
        }
        leg = service_transport_events(leg)?;
        leg = execute_actions(leg.attach_proto.tick(Instant::now()), leg)?;
        eager_ground_check(&mut leg)?;
        // ADR 0041 EndRun step 2 / Codex round-1 Blocker 1 discharge: the
        // LATCH drives teardown, not the ack -- "ack completion only
        // ACCELERATES teardown". `shutdown_requested` alone (the OLD,
        // ack-completion-only trigger via `AttachAction::Shutdown`, and the
        // transport-fatal self-end path) is not enough: a stalled ack, a
        // client that stops reading, a progress-deadline close, or a lost
        // connection must still tear this run down once the marker is
        // durable, exactly the cases ADR 0041 lists as unable to unlatch
        // it. The ack remains a courtesy -- serviced normally through
        // teardown (still tracked via `pending_sends`/the ack-grace window)
        // but never a precondition for STARTING it.
        if leg.shutdown_requested || leg.run_end_latched {
            break 'main ExitKind::Requested;
        }
        match commands.try_recv() {
            // Major 6 discharge: `Command::Kill` is the direct-caller/
            // supervisor own-behalf EndRun primitive (this module's own
            // doc on `Command`) -- it must carry a reason and route
            // through the SAME commit/latch transition as a wire
            // `shutdown`, or a resume could respawn a run that a caller
            // deliberately ended. Idempotent like every other caller of
            // `commit_run_end_marker`: a concurrent wire shutdown racing
            // this Kill still writes only one marker.
            Ok(Command::Kill) => {
                leg.shutdown_reason.get_or_insert_with(|| "operator_kill".to_string());
                commit_run_end_marker(
                    &mut leg.ctx,
                    &mut leg.w,
                    &mut leg.frames_written,
                    &mut leg.run_end_latched,
                    "operator_kill".to_string(),
                )?;
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
        // Switch-latency Phase 1 (c): a `Transport` event arriving DURING
        // this wait no longer waits out the full window before this loop
        // notices — `transport.0.set_wake`'s callback (registered above,
        // before `bind`) pushes `ReaderEvent::TransportActivity` on the
        // SAME channel this `recv_timeout` already blocks on, the instant
        // the transport queues a fresh event (AFTER queuing it — see that
        // callback's own doc — so `service_transport_events` at the top
        // of the NEXT iteration is guaranteed to find it). `Transport::
        // try_recv_event` itself is still never blocking (its own
        // contract, unchanged); this wait is what wakes early, not that
        // drain. `GROUP_COMMIT_WINDOW` stays the bound on how long output
        // may batch under sustained load and the cadence when nothing is
        // pending; with output pending, `OUTPUT_IDLE` of quiet commits it.
        match leg.output_rx.recv_timeout(output_wait(
            leg.pending_bytes,
            leg.last_commit.elapsed(),
            leg.last_output.elapsed(),
            leg.last_fsync.elapsed(),
        )) {
            Ok(ReaderEvent::Output(bytes)) => {
                leg.last_output = Instant::now();
                pace_output(bytes, &mut leg)?;
                leg = maybe_rotate(leg)?;
            }
            Ok(ReaderEvent::TransportActivity) => {
                leg.wake_pending.store(false, Ordering::Release);
                // Nothing else to do: `service_transport_events` at this
                // loop's own top (next iteration) drains and processes
                // whatever prompted this wake.
            }
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
                // Codex review (PR #227): `ReaderGone` (an explicit send,
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
        // Codex review (PR #227): checked here, after the match rather
        // than only inside its `Timeout` arm, so a transport wake (or any
        // other non-`Timeout` result) can never starve this deadline —
        // continuous transport activity every `recv_timeout` call used to
        // mean `last_commit.elapsed()` was never even read.
        if should_flush_output(leg.last_commit.elapsed(), leg.pending_bytes, leg.last_output.elapsed(), leg.last_fsync.elapsed()) {
            flush_output(&mut leg)?;
        }
    };
    Ok((leg, exit_kind))
}

pub(super) fn reap_domain<'t, P: Producer>(mut leg: Leg<'t, P>) -> Result<Leg<'t, P>> {
    // ONE teardown orchestrator (ADR 0041: "Teardown has ONE orchestrator")
    // for both exit_kind::ProducerExited and exit_kind::Requested — every
    // step below is unconditional: terminating an already-empty job is a
    // harmless no-op. `commands` is never read again from this point on —
    // real admission revocation (module doc), not receive-then-discard.
    //
    // Phase A: terminate the job, then REAP-POLL `ActiveProcesses` WHILE
    // STILL SERVICING `output_rx` (committing frames, answering the
    // handshake) AND the transport (mgmt/Sent, per finding 7) — review
    // finding, the blocker: the previous version polled the job with
    // nobody draining the channel, so a reader already blocked in
    // `OutputBudget::reserve` (or a DA1 only this loop could answer) could
    // leave `hOutput` undrained right when `ClosePseudoConsole` needed it
    // drained, and Microsoft's own docs say a pre-24H2 build's close can
    // wait indefinitely under exactly that condition.
    leg.producer.terminate_domain()?;
    let reap_deadline = Instant::now() + TEARDOWN_REAP_TIMEOUT;
    loop {
        service_transport_events_teardown(&mut leg)?;
        execute_teardown_actions(leg.attach_proto.tick(Instant::now()), &mut leg)?;
        eager_ground_check(&mut leg)?;
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
                leg = maybe_rotate(leg)?;
            }
            Ok(ReaderEvent::TransportActivity) => {
                // Switch-latency Phase 1 (c): same wake, same channel, as
                // the main loop's own arm -- mgmt/`Sent` traffic keeps
                // being serviced through teardown (finding 7), so it gets
                // the same early wake here rather than waiting out
                // `TEARDOWN_REAP_POLL`. `service_transport_events_teardown`
                // at this loop's own top does the actual draining.
                leg.wake_pending.store(false, Ordering::Release);
            }
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
                // Codex review (PR #227): see the main loop's identical arm
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
    flush_output(&mut leg)?;
    Ok(leg)
}

pub(super) fn drain_output<'t, P: Producer>(mut leg: Leg<'t, P>) -> Result<(Leg<'t, P>, std::thread::JoinHandle<()>)> {
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
            service_transport_events_teardown(&mut leg)?;
            execute_teardown_actions(leg.attach_proto.tick(Instant::now()), &mut leg)?;
            eager_ground_check(&mut leg)?;
            // Codex review (PR #227): checked here, unconditionally, every
            // iteration — mirroring Phase A's `reap_deadline` just above and
            // the main loop's own commit-deadline fix — rather than only
            // inside the `Timeout` arm below, where continuous transport
            // activity could starve it exactly as it did the commit deadline.
            if Instant::now() >= drain_deadline {
                return Err(Error::State(
                    "capsule_win: reader did not reach EOF within the teardown drain timeout".into(),
                ));
            }
            match leg.output_rx.recv_timeout(TEARDOWN_DRAIN_POLL) {
                Ok(ReaderEvent::Output(bytes)) => {
                    pace_output(bytes, &mut leg)?;
                    leg = maybe_rotate(leg)?;
                }
                Ok(ReaderEvent::TransportActivity) => {
                    // Switch-latency Phase 1 (c): same wake as both other
                    // sites -- see the main loop's own arm.
                    leg.wake_pending.store(false, Ordering::Release);
                }
                Ok(ReaderEvent::Done(_)) => {
                    // Round-2 review, finding 5: service transport ONE more
                    // time at the exact instant EOF ends this drain, so a
                    // status/mgmt request that arrived just after the last
                    // loop-top poll still gets answered while the pipe is
                    // provably still live -- without this, everything from
                    // here to `shutdown_all`'s eventual close (the flush and
                    // joins below, the exit-status wait, writing lifecycle
                    // state, sealing) is a live-but-unserviced pipe tail.
                    service_transport_events_teardown(&mut leg)?;
                    execute_teardown_actions(leg.attach_proto.tick(Instant::now()), &mut leg)?;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(ReaderEvent::ReaderGone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Codex review (PR #227): see the main loop's identical arm.
                    return Err(Error::State(
                        "capsule_win: the reader thread ended without a terminal Done event during drain".into(),
                    ));
                }
            }
        }
    } else {
        // The one thing the skipped drain's own EOF arm owes is the
        // final transport service at the EOF instant (round-2 finding
        // 5) -- pay it here instead, so a mgmt request that arrived
        // just after the last loop-top poll is still answered while the
        // pipe is provably live.
        service_transport_events_teardown(&mut leg)?;
        execute_teardown_actions(leg.attach_proto.tick(Instant::now()), &mut leg)?;
    }
    flush_output(&mut leg)?;
    Ok((leg, closer_handle))
}

pub(super) fn ack_grace<P: Producer>(leg: &mut Leg<'_, P>) -> Result<()> {
    // U1a, EndRun state machine item 4 / ack grace: the FINAL service poll
    // just above (the one at the exact EOF instant) can itself have
    // admitted a NEW mgmt `shutdown` and queued its `ShutdownAck` — give
    // that specific send up to `SHUTDOWN_ACK_GRACE` to be reported
    // physically written (removing its entry from `pending_sends`) before
    // this capsule's own transport goes away.
    //
    // U1a Codex round-1, Major 6 discharge: this window drains ONLY what
    // is ALREADY pending (`Sent`/`ConnectionClosed`, via
    // `drain_pending_sends_only`) — it admits NOTHING new
    // (`ConnectionOpened`/fresh `Bytes` are closed outright, never reaching
    // `attach_proto`). A request newly accepted with, say, 50ms left in
    // this window would have almost no time to get its own ack physically
    // written, contradicting the "final service poll" guarantee this grace
    // exists to honor — so after the ordinary teardown drain ends, no
    // request is newly admitted at all; only what is already outstanding
    // (the ack this grace exists for, or any other send already queued
    // when the drain ended) gets to finish.
    let shutdown_ack_deadline = Instant::now() + SHUTDOWN_ACK_GRACE;
    while leg.pending_sends
        .values()
        .any(|m| matches!(m, Some(SentMarker::ShutdownAck { .. })))
        && Instant::now() < shutdown_ack_deadline
    {
        drain_pending_sends_only(leg)?;
        execute_teardown_actions(leg.attach_proto.tick(Instant::now()), leg)?;
        std::thread::sleep(SHUTDOWN_ACK_GRACE_POLL);
    }
    Ok(())
}

pub(super) fn join_workers<P: Producer>(
    closer_handle: std::thread::JoinHandle<()>,
    reader_handle: std::thread::JoinHandle<()>,
    leg: &mut Leg<'_, P>,
) -> Result<()> {
    // The pipe's own disappearance: explicit HERE, rather than only
    // whenever `run` happens to return next (the exit-status wait and the
    // seal below need no pipe at all) — ADR 0041's grace is specifically
    // about DEFERRING that disappearance until it resolves, which requires
    // an actual close at THIS point, not a hope that returning soon is soon
    // enough. `shutdown_all` is idempotent (U1a): `ShutdownGuard`'s own
    // `Drop`, still ahead on every path, is a safe no-op the second time.
    //
    // Codex round-1 Blocker 3 discharge: ONE absolute aggregate deadline,
    // shared by the transport's OWN internal joins (accepted/reaper/every
    // connection worker, all cancellation-first per `Transport::
    // shutdown_all`'s own doc) AND this module's closer/reader threads —
    // "over an acceptor, a reaper, up to sixteen connection workers and
    // the capsule's own threads" (ADR 0041 bounds table). Cancellation for
    // THIS module's own threads already happened earlier in this same
    // function (Phase A's `producer.terminate_domain()`, Phase B's
    // `producer.close_output_side()` on `closer_handle`) — by this point
    // both threads are expected to be
    // at or near their own natural return, so `join_within` (never the
    // raw blocking `.join()`) is what actually bounds the residual gap
    // between "signalled EOF/exit" and "the thread function returned".
    // Expiry is TERMINAL: `run` must not seal-and-succeed, nor release the
    // writer fence (via `store`'s own drop), past a teardown that could
    // not prove every worker stopped — an `Err` here propagates before
    // `w.seal`/`store.advance_chain` are ever reached, and `store` (the
    // fence) still drops via its own destructor on this return path,
    // exactly as any other early `?` in this function already does.
    let teardown_deadline = Instant::now() + TEARDOWN_AGGREGATE_DEADLINE;
    let transport_ok = leg.transport.0.shutdown_all(teardown_deadline);
    let closer_ok = join_within(closer_handle, teardown_deadline);
    let reader_ok = join_within(reader_handle, teardown_deadline);
    if !(transport_ok && closer_ok && reader_ok) {
        return Err(Error::State(format!(
            "capsule_win: aggregate teardown did not complete within its {TEARDOWN_AGGREGATE_DEADLINE:?} \
             deadline (transport ok={transport_ok}, closer ok={closer_ok}, reader ok={reader_ok}); \
             refusing to seal or report success past an unproven teardown"
        )));
    }
    Ok(())
}

pub(super) fn seal_run<P: Producer>(exit_kind: ExitKind, producer_uptime_ms: u64, mut leg: Leg<'_, P>) -> Result<ExitSummary> {
    // Step 5: the producer's own exit status, raw and unsigned end-to-end
    // for the Windows `Code` case (review finding: a Unix-style `i32` cast
    // would turn a high-bit NTSTATUS-shaped code negative for no reason).
    // `wait()` first establishes the honesty-bound precondition
    // `exit_status_after_confirmed_exit`'s own doc requires —
    // `domain_is_empty` above already proved the process isn't running,
    // but this satisfies the bound by the letter of its doc, not just by
    // inference.
    if !leg.producer.wait(Duration::from_secs(5))? {
        return Err(Error::State(
            "capsule_win: producer did not signal after its domain reaped to empty".into(),
        ));
    }
    let exit_status = leg.producer.exit_status_after_confirmed_exit()?;

    // The mgmt `shutdown` reason, if that is what drove this EndRun (ADR
    // 0041: "the reason string is recorded in producer_dead's detail").
    // `producer_uptime_ms` (N1, captured well above, at the exit_kind
    // boundary -- NOT recomputed here, past all the teardown machinery
    // this point sits after) is an ADDITIVE, free-form diagnostic field
    // -- like `reason` already is -- not a registered ADR 0039 feature:
    // it changes no authority, so no segment needs to declare anything
    // to carry it, and an older reader simply ignores an unknown plain
    // JSON field, exactly as `detail` has always allowed.
    //
    // ADR 0043 decision 13: `Code(c)` writes the SAME `exit_code` (u32)
    // field this crate has always written (Windows never reaches the
    // other arm); `Signal(n)` is the additive Unix shape, `signal` (i32),
    // unreachable here.
    let mut detail = json!({ "producer_uptime_ms": producer_uptime_ms });
    match exit_status {
        ExitStatus::Code(c) => detail["exit_code"] = json!(c),
        ExitStatus::Signal(n) => detail["signal"] = json!(n),
    }
    if let Some(reason) = &leg.shutdown_reason {
        detail["reason"] = json!(reason);
    }
    // ADR 0043 decision 12, as amended: the output side ended before
    // teardown closed it AND the producer's own exit explained it inside
    // `READER_END_EXIT_GRACE` -- the one case the pre-close rule now admits,
    // and it is admitted RECORDED, never silently. Additive and free-form
    // exactly like `reason` above, absent unless that case occurred: on
    // macOS it is the kernel's revoke on the session leader's exit (where
    // the producer's last undrained output can be lost with it, which is
    // precisely why the record says so); on Linux and Windows the arms that
    // set it are unreachable, so no record written there ever carries it.
    if let Some(how) = &leg.output_ended_early {
        detail["output_ended_early"] = json!(how);
    }
    let f = leg.ctx.capsule_frame(Class::Lifecycle, json!({"kind": "producer_dead", "detail": detail}));
    leg.w.append(&f, Commit::Immediate)?;
    leg.frames_written += 1;

    let digest = leg.w.seal(None)?;
    leg.store.advance_chain(digest);
    leg.segments_sealed += 1;

    Ok(ExitSummary {
        exit_code: Some(exit_status),
        exit_kind,
        frames_written: leg.frames_written,
        segments_sealed: leg.segments_sealed,
        handshake_answered: leg.dsr_answered,
        handshake_suppressed_matches: leg.handshake_suppressed_matches,
        resize_os_calls: leg.resize_os_calls,
    })
}
