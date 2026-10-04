//! The loop's output path: the reader thread, recording and pacing output, the commit watermark and segment rotation. The byte budget and the commit-timing rules are capsule/output.rs.
use super::lanes::execute_light_actions;
use super::*;

pub(super) fn spawn_reader<P: Producer>(
    producer: &mut P,
    output_budget: &Arc<OutputBudget>,
    tx: mpsc::Sender<ReaderEvent>,
) -> std::thread::JoinHandle<()> {
    // Three senders share this channel, not one: the reader thread below,
    // `Transport::set_wake`'s callback (registered earlier, sends
    // `TransportActivity`), and this thread's own `ReaderGoneGuard` (sends
    // `ReaderGone` on every exit, including a panic unwind). No bridging
    // threads for input/control though (see the module doc): `commands`
    // is serviced directly, by this loop, from its own separate receiver.
    // `take_output` runs EXACTLY here — before this thread starts, per its
    // own doc — so `producer` itself stays a live, fully-owned binding for
    // every other call this function makes (`input`/`resize`/`wait`/...).
    // `tx`/`output_rx` themselves were created earlier, ahead of
    // `transport.0.bind` (switch-latency Phase 1 (c)) — `tx` is moved
    // into this thread's closure below exactly as before; only its
    // CREATION moved, not its ownership story.
    let mut reader = producer.take_output();
    let budget = Arc::clone(&output_budget);
    std::thread::spawn(move || {
        // a drop guard, not another explicit
        // send at the bottom of this closure — the two designed exits
        // already send `Done` and return, but a `read()`/`budget` call
        // panicking partway through would skip any send placed after
        // it. `Drop` runs on every exit, unwind included, which is the
        // one guarantee an ordinary send can't make; see `ReaderGone`'s
        // own doc for why this needs to be a real, matched event
        // rather than relying on the channel's sender count.
        struct ReaderGoneGuard(mpsc::Sender<ReaderEvent>);
        impl Drop for ReaderGoneGuard {
            fn drop(&mut self) {
                let _ = self.0.send(ReaderEvent::ReaderGone);
            }
        }
        let _reader_gone_guard = ReaderGoneGuard(tx.clone());
        let mut buf = [0u8; READ_CHUNK];
        loop {
            if !budget.reserve(READ_CHUNK as u64) {
                // Cancelled: `run` is already exiting some other way.
                // Nothing left to report; just stop.
                return;
            }
            match reader.read(&mut buf) {
                Ok(0) => {
                    budget.release(READ_CHUNK as u64);
                    let _ = tx.send(ReaderEvent::Done(Ok(())));
                    return;
                }
                // a signal-interrupted read is not
                // an end of stream on ANY platform -- the ConPTY
                // producer never actually produces this (Windows has
                // no equivalent signal-delivery-during-read
                // interruption for a named pipe read), but the Unix
                // pty producer's plain `File` can, any time the
                // reading thread receives a signal (this crate's own
                // `Drop`-time `killpg`/`waitpid` and the reap-bound
                // polling elsewhere don't target this thread, but an
                // operator/OS signal targeting the whole process
                // would). Release the reservation and retry the SAME
                // read rather than treating it as terminal -- the loop
                // re-reserves at its own top.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    budget.release(READ_CHUNK as u64);
                    continue;
                }
                Err(e) => {
                    budget.release(READ_CHUNK as u64);
                    let _ = tx.send(ReaderEvent::Done(Err(e)));
                    return;
                }
                Ok(n) => {
                    let n = n as u64;
                    if n < READ_CHUNK as u64 {
                        budget.release(READ_CHUNK as u64 - n);
                    }
                    if tx.send(ReaderEvent::Output(buf[..n as usize].to_vec())).is_err() {
                        budget.release(n);
                        return;
                    }
                }
            }
        }
    })
}

pub(super) fn flush_output<P: Producer>(leg: &mut Leg<'_, P>) -> Result<()> {
    if leg.pending_bytes > 0 {
        leg.w.commit()?; // the watermark: fsync BEFORE anything is published
        leg.last_fsync = Instant::now();
        execute_light_actions(leg.attach_proto.output_committed(&leg.pending_output, Instant::now()), leg);
        leg.pending_output.clear();
        leg.pending_bytes = 0;
    } else if leg.w.has_unsynced() {
        // A Buffered input-WAL record with no output behind it: commit it
        // by the next group-commit check (ADR 0039 Durability invariants);
        // nothing is published, so no `output_committed`.
        leg.w.commit()?;
        leg.last_fsync = Instant::now();
    }
    leg.last_commit = Instant::now();
    // ADR 0041: attach is GROUND-GATED; the watermark barrier
    // (force pending commit -> publish to EXISTING subscribers ->
    // checkpoint -> subscribe) is exactly this ordering -- publish
    // above already ran, so a ground boundary found HERE is the
    // single loop step the barrier requires.
    if leg.parser.is_ground() {
        execute_light_actions(leg.attach_proto.ground_reached(Instant::now()), leg);
    }
    Ok(())
}

pub(super) fn maybe_rotate<'t, P: Producer>(mut leg: Leg<'t, P>) -> Result<Leg<'t, P>> {
    if leg.seg_bytes >= SEGMENT_MAX_BYTES {
        flush_output(&mut leg)?;
        let digest = leg.w.seal(None)?;
        leg.store.advance_chain(digest);
        leg.segments_sealed += 1;
        leg.w = leg.store.open_segment_with_features(wall_ms(), leg.segment_features.clone())?;
        leg.seg_bytes = 0;
    }
    Ok(leg)
}

/// Bounds consecutive output work to one `GROUP_COMMIT_BYTES` worth
/// (the SAME threshold the writer already paces its own fsyncs by)
/// before yielding this thread -- the loop-fairness fix above. Takes
/// `bytes` itself (not a pre-computed length) so every call site reads
/// as "handle this chunk, paced" in one line: `.len()` borrows before
/// `handle_output` moves it.
/// Offers a scheduling window to another ready thread every
/// `GROUP_COMMIT_BYTES` worth of output -- a timed sleep (see
/// the doc above `bytes_since_yield`), not a bound: it may do nothing.
pub(super) fn pace_output<P: Producer>(bytes: Vec<u8>, leg: &mut Leg<'_, P>) -> Result<()> {
    leg.bytes_since_yield += bytes.len();
    handle_output(bytes, leg)?;
    if leg.bytes_since_yield >= GROUP_COMMIT_BYTES {
        std::thread::sleep(Duration::from_millis(1));
        leg.bytes_since_yield = 0;
    }
    Ok(())
}

/// Attaching to an idle session (real CI failure, windows-latest
/// only): `ground_reached` was previously fed ONLY from
/// `flush_output`, itself reached only by fresh output crossing the
/// group-commit threshold, or a periodic idle check gated behind the
/// OUTPUT CHANNEL's own `recv_timeout` cadence — never directly by
/// admission, and never by `tick`, the one hook this loop already
/// calls unconditionally every iteration. An attach landing on an
/// ALREADY-idle, already-at-ground session (a shell sitting at its
/// prompt — the ordinary case, exercised once the fidelity test's
/// producer goes silent after `--linger`) depended entirely on that
/// separate cadence happening to notice, which is exactly the kind of
/// dependency `pace_output`'s own history above already proved
/// fragile on a loaded windows-latest runner: the attach pended for
/// the full 5 s `GroundTimeout` and was refused instead of completing
/// on the very next iteration.
///
/// Called every iteration, right after `tick`, so it runs in the SAME
/// iteration an attach was just admitted in (a) and on every
/// subsequent iteration while one still pends (b) — no separate
/// cadence to depend on. Scoped behind `ground_gate_pending()` (a
/// cheap check) so the vastly more common "nothing pending" iteration
/// pays nothing beyond it. Watermark semantics stay exact: with no
/// pending uncommitted bytes, NOW already is a valid commit boundary
/// (`flush_output` skips the commit but still evaluates ground); with
/// some pending, `flush_output` forces the SAME commit-then-check
/// barrier it always runs, just immediately rather than waiting for
/// the group-commit threshold or the idle timer to get to it.
pub(super) fn eager_ground_check<P: Producer>(leg: &mut Leg<'_, P>) -> Result<()> {
    if leg.attach_proto.ground_gate_pending() {
        flush_output(leg)?;
    }
    Ok(())
}

// One producer-output handler, used identically pre-teardown AND
// during BOTH teardown phases — so "the handshake keeps answering
// through the drain" (ADR 0041) can't be missed by one call site and
// not the other. Feeds the live parser, answers the FIRST DA1 query
// ever seen (recording request -> response -> outcome), records the
// raw producer frame, and tracks the group-commit/echo state.
fn handle_output<P: Producer>(bytes: Vec<u8>, leg: &mut Leg<'_, P>) -> Result<()> {
    leg.parser.process(&bytes);

    use base64_engine::encode_b64;
    let f = leg.ctx.producer_frame(json!({"bytes_b64": encode_b64(&bytes)}));
    leg.w.append(&f, Commit::Buffered)?;
    leg.frames_written += 1;
    leg.seg_bytes += bytes.len() as u64 + 128;
    leg.output_budget.release(bytes.len() as u64);
    leg.pending_output.extend_from_slice(&bytes);
    leg.pending_bytes += bytes.len();
    if leg.pending_bytes >= GROUP_COMMIT_BYTES {
        flush_output(leg)?;
    }

    let matches = leg.handshake.feed(&bytes);
    if matches > 0 {
        if !leg.dsr_answered {
            leg.dsr_answered = true;
            // Query exchange, ADR 0041's own phrase and shape:
            // request -> response (only on a successful write) ->
            // outcome (always, reflecting whether it was).
            let req = leg.ctx.capsule_frame(
                Class::ControlExchange,
                json!({"phase": "request", "kind_ns": "conpty/host-handshake",
                       "to": {"kind": "producer"}, "body": {"query": "da1"}}),
            );
            let req_seq = req.seq;
            leg.w.append(&req, Commit::Immediate)?;
            leg.frames_written += 1;

            let write_result = leg.producer.input().write_all(host_handshake::DA1_REPLY);
            if write_result.is_ok() {
                let mut resp = leg.ctx.capsule_frame(
                    Class::ControlExchange,
                    json!({"phase": "response", "kind_ns": "conpty/host-handshake",
                           "body": {"query": "da1"}}),
                );
                resp.refs = vec![FrameRef { kind: RefKind::RespondsTo, frame: req_seq }];
                leg.w.append(&resp, Commit::Immediate)?;
                leg.frames_written += 1;
            }
            let outcome_body = match &write_result {
                Ok(()) => json!({"disposition": "ok"}),
                Err(e) => json!({"disposition": "failed", "reason": e.to_string()}),
            };
            let out = leg.ctx.capsule_frame(
                Class::ControlExchange,
                json!({"phase": "outcome", "kind_ns": "conpty/host-handshake", "scope": "pty",
                       "target": format!("{}:{}", req_seq.epoch, req_seq.n), "body": outcome_body}),
            );
            leg.w.append(&out, Commit::Immediate)?;
            leg.frames_written += 1;

            // Any FURTHER matches in this SAME chunk are already
            // "later" than the one just answered.
            leg.handshake_suppressed_matches += (matches - 1) as u64;
        } else {
            leg.handshake_suppressed_matches += matches as u64;
        }
    }
    Ok(())
}
