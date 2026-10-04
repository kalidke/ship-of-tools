//! The SOSV lane's connection state machine: pending closes, per-connection state and the service tick.

use crate::supervisor::*;

// ---------------------------------------------------------------------
// The supervisor lane's own connection state machine
// ---------------------------------------------------------------------

/// A refusal (or `stop`) reply queued as a connection's LAST word waits
/// through TWO stages before actually closing: first for
/// `LaneEvent::Sent` (the write has physically completed) or a
/// bounded deadline if it never arrives, THEN an additional flush-grace
/// window so the client's own `read()` has a real chance to drain the
/// bytes before this end tears the connection down.
enum PendingClose {
    AwaitingSent { deadline: Instant },
    FlushGrace { close_at: Instant },
}

pub(in crate::supervisor) struct Conn {
    splitter: wire::FrameSplitter,
    hello_ok: bool,
    last_activity: Instant,
    pending_close: Option<PendingClose>,
}

/// Bundles everything `handle_lane_bytes`/`service_lane` need beyond the
/// transport and per-connection state itself — keeps both functions'
/// own argument counts small (clippy's own `too_many_arguments`). A
/// `reset`'s own admission only ever starts `spawn_reset` (the pointer
/// rename/bootstrap/publish, needing nothing about the NEXT leg to
/// spawn) — respawning after it completes happens later, in the main
/// loop's own `Resetting -> Spawning` transition, which already has
/// `capsule_exe`/`lease`/`config` in scope directly.
pub(in crate::supervisor) struct LaneCtx<'a> {
    pub(in crate::supervisor) authority: &'a mut AuthorityState,
    pub(in crate::supervisor) lifecycle: &'a mut Lifecycle,
}

/// Services the lane's event queue once. Returns `true` iff the accept
/// loop has died PERMANENTLY (`LaneEvent::AcceptError` — the
/// transport's own doc: "stopped accepting new connections FOR GOOD"),
/// which the caller treats as terminal.
///
/// `first`, if `Some`, is an event the main loop's own tail wait already
/// pulled off `lane.events()` (via `recv_timeout`, to wake early on lane
/// activity — see that call site) — processed here as this tick's OWN
/// first event, ahead of any further `try_recv`, rather than a second,
/// redundant fetch. `None` whenever that wait simply timed out idle —
/// this function then behaves exactly as it did before that wake path
/// existed.
pub(in crate::supervisor) fn service_lane(
    lane: &Lane,
    mut first: Option<LaneEvent>,
    conns: &mut HashMap<ConnId, Conn>,
    ctx: &mut LaneCtx,
    now: Instant,
) -> bool {
    let mut accept_loop_dead = false;
    // bounded to LANE_EVENT_QUOTA per tick —
    // an earlier version drained the WHOLE channel unconditionally, so
    // sustained lane traffic (each `Bytes` event triggering its own
    // `handle_lane_bytes` call) could keep this loop running
    // indefinitely, starving `Lifecycle` polling, worker results,
    // watchdogs, and `Terminal` grace of their own turn. Leftover
    // events stay queued in the transport's own channel for the NEXT
    // tick — no extra bookkeeping needed here. `first` (if any) counts
    // against this SAME budget — it is one event already off the
    // channel, not an addition to it.
    for _ in 0..LANE_EVENT_QUOTA {
        let event = match first.take() {
            Some(ev) => ev,
            None => match lane.events().try_recv() {
                Ok(ev) => ev,
                Err(_) => break,
            },
        };
        match event {
            LaneEvent::Accepted(id) => {
                conns.insert(
                    id,
                    Conn { splitter: wire::FrameSplitter::new(), hello_ok: false, last_activity: now, pending_close: None },
                );
            }
            LaneEvent::Bytes(id, bytes) => {
                handle_lane_bytes(lane, conns, id, &bytes, ctx, now);
            }
            LaneEvent::Closed(id, _reason) => {
                conns.remove(&id);
            }
            LaneEvent::Sent(id, _marker) => {
                // a `stop` reply is
                // tracked through this SAME per-connection mechanism,
                // not a second bespoke one — see `CommandEffect::Stop`'s
                // own handling in `handle_lane_bytes` and
                // `AuthorityState::stop_requested`'s own doc for how the
                // main loop's own exit condition reads it back out.
                if let Some(conn) = conns.get_mut(&id) {
                    if matches!(conn.pending_close, Some(PendingClose::AwaitingSent { .. })) {
                        conn.pending_close = Some(PendingClose::FlushGrace { close_at: now + REFUSAL_FLUSH_GRACE });
                    }
                }
            }
            LaneEvent::AcceptError(e) => {
                note(format_args!("supervisor lane accept loop failed permanently: {e}"));
                accept_loop_dead = true;
            }
        }
    }
    let mut to_close: Vec<ConnId> = Vec::new();
    for (id, conn) in conns.iter() {
        let idle = now.saturating_duration_since(conn.last_activity) >= LANE_IDLE_DEADLINE;
        let close_due = match &conn.pending_close {
            Some(PendingClose::AwaitingSent { deadline }) => {
                if now >= *deadline {
                    note(format_args!("a refusal reply was never confirmed sent; closing anyway"));
                }
                now >= *deadline
            }
            Some(PendingClose::FlushGrace { close_at }) => now >= *close_at,
            None => idle,
        };
        if close_due {
            to_close.push(*id);
        }
    }
    for id in to_close {
        lane.close(id);
        conns.remove(&id);
    }
    accept_loop_dead
}

/// the ONE thing that must
/// be bounded per tick is HOW LONG `service_lane`'s own event-drain loop
/// runs before returning control — capped there, at `LANE_EVENT_QUOTA`
/// (`lane/pipe_win/conn.rs`'s own `reader_loop` is a tight, unpaced
/// `ReadFile`-then-`deliver_bytes`-then-loop with nothing gating a
/// sustained single connection's throughput, so `MAX_LANE_INSTANCES`
/// alone does not bound it — a genuine, not merely theoretical, per-tick
/// starvation risk). EACH `Bytes` event this function processes is
/// itself already bounded to `transport::READ_BUF_LEN` (64 KiB) by the
/// transport, so bounding events-per-tick already transitively bounds
/// frames-per-tick too.
fn handle_lane_bytes(lane: &Lane, conns: &mut HashMap<ConnId, Conn>, id: ConnId, bytes: &[u8], ctx: &mut LaneCtx, now: Instant) {
    let mut close_after = false;
    let mut pending: Option<PendingClose> = None;
    {
        let Some(conn) = conns.get_mut(&id) else { return };
        conn.last_activity = now;
        let (frames, err) = conn.splitter.feed(bytes);
        for frame in frames {
            match frame {
                DecodedFrame::SupervisorRequest(SupervisorRequest::Hello { proto, build: _ }) if !conn.hello_ok => {
                    // ADR 0045 decision 7: the gate is the protocol
                    // integer alone; `build` rides the wire as
                    // information only and is never compared.
                    if proto != wire::SUPERVISOR_PROTO_V1 {
                        note(format_args!("hello refused: lane proto {proto}, ours {}", wire::SUPERVISOR_PROTO_V1));
                        let reply = wire::encode_supervisor_reply(&SupervisorReply::Refused {
                            reason: wire::SupervisorRefusedReason::VersionSkew,
                        })
                        .expect("Refused encodes unconditionally");
                        match lane.send(id, reply, Some(id)) {
                            Ok(()) => pending = Some(PendingClose::AwaitingSent { deadline: now + REFUSAL_SENT_DEADLINE }),
                            Err(_) => close_after = true,
                        }
                        break;
                    }
                    conn.hello_ok = true;
                    let (pid, created) = self_pid_and_created().unwrap_or((0, 0));
                    let reply = wire::encode_supervisor_reply(&SupervisorReply::HelloOk {
                        proto: wire::SUPERVISOR_PROTO_V1,
                        build: crate::identity::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
                        pid,
                        created,
                    })
                    .expect("HelloOk's build is this crate's own bounded constant");
                    let _ = lane.send(id, reply, None);
                }
                DecodedFrame::SupervisorRequest(SupervisorRequest::Hello { .. }) => {
                    close_after = true;
                    break;
                }
                DecodedFrame::SupervisorRequest(_) if !conn.hello_ok => {
                    close_after = true;
                    break;
                }
                DecodedFrame::SupervisorRequest(req @ (SupervisorRequest::Status | SupervisorRequest::Query { .. })) => {
                    let reply = ctx.authority.handle_status_or_query(ctx.lifecycle, req);
                    if let SupervisorReply::Operation(state) = &reply {
                        if is_journal_unreadable(state) {
                            force_terminal(ctx.lifecycle, &mut ctx.authority.retired_legs, "the operation journal became unreadable".into());
                        }
                    }
                    let bytes = encode_reply_or_fallback(&reply);
                    let _ = lane.send(id, bytes, None);
                }
                DecodedFrame::SupervisorRequest(SupervisorRequest::Command { operation_id, op }) => {
                    let outcome: Option<SupervisorOperationState> =
                        match ctx.authority.handle_command(ctx.lifecycle, operation_id, op) {
                            Ok(CommandEffect::EndRun { operation_id, epoch, reason }) => {
                                let voyage_id =
                                    ctx.authority.voyage_id.clone().expect("EndRun was admitted, so voyage_id is Some");
                                let (rx, handle) =
                                    spawn_end_run(ctx.authority.state_dir.clone(), operation_id.clone(), voyage_id, epoch, reason);
                                // EndRun is only ever admitted from `Ready`
                                // (`handle_command`'s own check, just above)
                                // -- extract its retained `process` so
                                // `Ending` can carry it forward, rather
                                // than dropping it in the same assignment
                                // that replaces `*ctx.lifecycle` (see `Lifecycle::Ending`'s
                                // own doc for why).
                                let process = match std::mem::replace(ctx.lifecycle, Lifecycle::EndedNoRespawn) {
                                    Lifecycle::Ready { process } => process,
                                    _ => unreachable!("EndRun is only ever admitted from Ready"),
                                };
                                *ctx.lifecycle = Lifecycle::Ending {
                                    operation_id,
                                    rx,
                                    handle,
                                    started_at: now,
                                    pending_reply: Some(id),
                                    process,
                                };
                                // the reply is DEFERRED to record_closed — never sent here.
                                None
                            }
                            Ok(CommandEffect::Reset { operation_id, new_voyage, aside }) => {
                                let (rx, handle) =
                                    spawn_reset(ctx.authority.state_dir.clone(), operation_id.clone(), new_voyage, aside);
                                *ctx.lifecycle = Lifecycle::Resetting { operation_id, rx, handle, started_at: now };
                                Some(SupervisorOperationState::Accepted)
                            }
                            Ok(CommandEffect::Stop { reply }) => {
                                // `stop` no
                                // longer transitions the Lifecycle AT
                                // ALL — it stays exactly whatever it
                                // already was, resolving itself through
                                // its own normal transition arms in the
                                // main loop (worker ownership, panic
                                // detection, watchdogs, all UNCHANGED).
                                // Its reply is delivery-gated through the
                                // SAME per-connection `PendingClose` gate
                                // every other reply already uses — never
                                // a second bespoke mechanism — so it is
                                // sent HERE, inline, rather than falling
                                // through to the shared tail below (which
                                // never marker-tracks a reply). Whether
                                // the journal write failed is read
                                // straight off `reply`'s own shape.
                                let journal_failed = matches!(&reply, SupervisorOperationState::Failed { .. });
                                let wire_reply = SupervisorReply::Operation(reply);
                                let reply_bytes = encode_reply_or_fallback(&wire_reply);
                                match lane.send(id, reply_bytes, Some(id)) {
                                    Ok(()) => pending = Some(PendingClose::AwaitingSent { deadline: now + REFUSAL_SENT_DEADLINE }),
                                    Err(_) => close_after = true,
                                }
                                let terminal_now = matches!(ctx.lifecycle, Lifecycle::Terminal { .. }) || journal_failed;
                                match &mut ctx.authority.stop_requested {
                                    Some(existing) => existing.terminal_severity |= terminal_now,
                                    None => {
                                        ctx.authority.stop_requested =
                                            Some(StopRequested { primary_conn: id, terminal_severity: terminal_now });
                                    }
                                }
                                None // already sent (marker-tracked), above
                            }
                            Err(state) => Some(state),
                        };
                    if let Some(state) = outcome {
                        if is_journal_unreadable(&state) {
                            force_terminal(ctx.lifecycle, &mut ctx.authority.retired_legs, "the operation journal became unreadable".into());
                        }
                        let reply = SupervisorReply::Operation(state);
                        let bytes = encode_reply_or_fallback(&reply);
                        let _ = lane.send(id, bytes, None);
                    }
                }
                _ => {
                    close_after = true;
                    break;
                }
            }
            if close_after {
                break;
            }
        }
        if err.is_some() {
            close_after = true;
        }
        if let Some(p) = pending {
            conn.pending_close = Some(p);
        }
    }
    if close_after {
        lane.close(id);
        conns.remove(&id);
    }
}

