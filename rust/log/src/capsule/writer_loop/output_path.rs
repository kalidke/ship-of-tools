//! The loop's output path: the reader thread, handling and pacing output, and the ground gate. The byte budget is capsule/output.rs.
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

/// Evaluates the ground gate: a pending attach is promoted at a parser ground boundary, after every byte the parser
/// has seen was published (`handle_output` publishes before it asks), so the checkpoint and the bytes after it
/// agree on where they meet.
pub(super) fn ground_check<P: Producer>(leg: &mut Leg<'_, P>) {
    if leg.parser.is_ground() {
        execute_light_actions(leg.attach_proto.ground_reached(Instant::now()), leg);
    }
}

/// Handles one chunk of output, and offers a scheduling window to another ready thread every `PACE_BYTES` worth of
/// output: a timed sleep (see the doc at `bytes_since_yield`'s initialization in `start`), not a bound.
pub(super) fn pace_output<P: Producer>(bytes: Vec<u8>, leg: &mut Leg<'_, P>) -> Result<()> {
    leg.bytes_since_yield += bytes.len();
    handle_output(bytes, leg)?;
    if leg.bytes_since_yield >= PACE_BYTES {
        std::thread::sleep(Duration::from_millis(1));
        leg.bytes_since_yield = 0;
    }
    Ok(())
}

/// Attaching to an idle session (real CI failure, windows-latest only): the ground gate was fed only by fresh output,
/// so an attach landing on an already-idle, already-at-ground session (a shell sitting at its prompt, the ordinary
/// case) pended for the full 5 s `GroundTimeout` and was refused instead of completing on the very next iteration.
///
/// Called every iteration, right after `tick`, so it runs in the SAME iteration an attach was just admitted in and on
/// every subsequent iteration while one still pends. Scoped behind `ground_gate_pending()` (a cheap check) so the
/// vastly more common "nothing pending" iteration pays nothing beyond it. Every byte the parser has seen was already
/// published, so NOW is a valid boundary whenever the parser is at ground.
pub(super) fn eager_ground_check<P: Producer>(leg: &mut Leg<'_, P>) {
    if leg.attach_proto.ground_gate_pending() {
        ground_check(leg);
    }
}

// One producer-output handler, used identically pre-teardown AND during BOTH teardown phases, so "the handshake
// keeps answering through the drain" (ADR 0041) can't be missed by one call site and not the other. Feeds the live
// parser, publishes the bytes to the attached viewers, answers the FIRST DA1 query ever seen, and marks the screen
// file's content changed.
fn handle_output<P: Producer>(bytes: Vec<u8>, leg: &mut Leg<'_, P>) -> Result<()> {
    leg.parser.process(&bytes);
    leg.output_budget.release(bytes.len() as u64);
    if let Some(signals) = &leg.signals {
        signals.output();
    }
    if let Some(screen) = &mut leg.screen {
        screen.output_arrived();
    }
    execute_light_actions(leg.attach_proto.output_committed(&bytes, Instant::now()), leg);
    ground_check(leg);
    if let Some(row) = &leg.row {
        row.output(&bytes, &leg.parser);
    }

    let matches = leg.handshake.feed(&bytes);
    if matches > 0 {
        if !leg.dsr_answered {
            leg.dsr_answered = true;
            // Answer the query, through the one owner of terminal writes (the loop never blocks on the terminal): a reply
            // that cannot be queued is noted, never fatal (the producer may be gone already).
            if !leg.input.write_detached(host_handshake::DA1_REPLY.to_vec()) {
                leg_note(format_args!("sot-capsule: the host handshake reply was not queued"));
            }
            // Any FURTHER matches in this SAME chunk are already "later" than the one just answered.
            leg.handshake_suppressed_matches += (matches - 1) as u64;
        } else {
            leg.handshake_suppressed_matches += matches as u64;
        }
    }
    Ok(())
}
