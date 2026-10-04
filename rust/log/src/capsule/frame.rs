//! The writer loop's frame factory, the run-end marker and the input WAL, and `wall_ms`, the
//! wall clock the capsule, `claude.rs` and the fault writer share.
use super::*;

/// Encodes a wire `idem_key` (16 raw bytes) as the lowercase hex32 shape
/// ADR 0039's `input` frame requires.
fn hex_idem_key(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The writer loop's frame factory: sequential seq, capsule clocks, and the
/// per-run refs (attached_to / input WAL) threaded through one small state.
pub(super) struct FrameCtx {
    pub(super) epoch: u64,
    pub(super) next_n: u64,
    pub(super) t0: Instant,
    /// The DURABLE take-epoch value — the capsule's own, fed in/out of
    /// `attach_proto` as a plain value (ADR 0041: "the durable holder/epoch
    /// is the CAPSULE's"). Starts at the null-holder revoke's value; the
    /// step-4 "local" grant is deleted (spec gate) — the first driver ever
    /// is a pipe `take`.
    pub(super) take_epoch: u64,
    /// The DURABLE holder's controller_id, mirroring `take_epoch` — `None`
    /// until the first real `take` commits.
    pub(super) holder: Option<String>,
    pub(super) attached: Option<Seq>,
}

impl FrameCtx {
    fn seq(&mut self) -> Seq {
        let s = Seq {
            epoch: self.epoch,
            n: self.next_n,
        };
        self.next_n += 1;
        s
    }
    fn mono_us(&self) -> u64 {
        self.t0.elapsed().as_micros() as u64
    }
    pub(super) fn capsule_frame(&mut self, class: Class, payload: serde_json::Value) -> Envelope {
        Envelope {
            seq: self.seq(),
            class,
            source: Source {
                emitter: Emitter::Capsule,
                actor: Actor {
                    kind: ActorKind::Unknown,
                    controller_id: None,
                    take_epoch: None,
                },
                derivation: Derivation::Synthetic,
            },
            t_wall_ms: wall_ms(),
            t_mono_us: self.mono_us(),
            stream: None,
            transformed: None,
            refs: vec![],
            payload: Some(payload),
            payload_ref: None,
        }
    }
    /// A controller-actor frame declaring EXACTLY `(controller_id,
    /// take_epoch)` — a thin constructor, not itself the source of which
    /// epoch is honest. Every
    /// real call site (`run_input_wal`) now passes the COMMITTED
    /// `ctx.take_epoch`, never the wire-claimed one — a stale input's
    /// CLAIMED epoch is recorded only inside the `refused_stale_epoch`
    /// fact's own diagnostic body, never on this envelope's actor
    /// identity (ADR 0039's take predicate judges staleness from the
    /// fact, not from a claimed-epoch envelope this module no longer
    /// writes).
    fn controller_frame(
        &mut self,
        class: Class,
        controller_id: String,
        take_epoch: u64,
        payload: serde_json::Value,
    ) -> Envelope {
        let mut e = self.capsule_frame(class, payload);
        e.source.actor = Actor {
            kind: ActorKind::Controller,
            controller_id: Some(controller_id),
            take_epoch: Some(take_epoch),
        };
        e
    }
    /// A controller-actor frame using THIS ctx's own current durable
    /// identity — correct only where the actor IS, by construction, the
    /// current holder (e.g. `resize`, driver-only and carrying no identity
    /// fields of its own on the wire).
    pub(super) fn current_controller_frame(&mut self, class: Class, payload: serde_json::Value) -> Envelope {
        let controller_id = self.holder.clone().unwrap_or_default();
        let take_epoch = self.take_epoch;
        self.controller_frame(class, controller_id, take_epoch, payload)
    }
    pub(super) fn producer_frame(&mut self, payload: serde_json::Value) -> Envelope {
        let mut e = self.capsule_frame(Class::Producer, payload);
        e.source.emitter = Emitter::Producer;
        e.source.actor.kind = ActorKind::Producer;
        e.source.derivation = Derivation::Native;
        e.refs = vec![FrameRef {
            kind: RefKind::AttachedTo,
            frame: self.attached.expect("attached before producer frames"),
        }];
        e
    }
}

/// ADR 0041 EndRun steps 1-2: append + fsync the ONE
/// `run_end_requested {reason}` lifecycle frame and IRREVOCABLY latch
/// EndRun — idempotent past the first (step 4: first commit wins, a
/// concurrent later request writes no second marker). Shared by BOTH of
/// `run`'s action executors (`execute_actions` and
/// `execute_teardown_actions`) since a `shutdown` arriving during the
/// final teardown poll must latch exactly the same way as one arriving
/// mid-run — see this crate's `verify::leg_carries_run_end_marker` for
/// the READ half a later unit's respawn decision uses.
///
/// A real function, not a macro, specifically so it is independently
/// testable against a plain `SegmentWriter`/`FrameCtx` pair (this
/// module's own `tests`), with no real ConPTY run needed to exercise the
/// one property that matters here: on an append failure, `?` propagates
/// with `run_end_latched` left false — no ack ever reached (it sits
/// AFTER this call in the same action batch, unreached once `?` returns)
/// and nothing is latched, exactly ADR 0039's crash shape ("no ack, no
/// marker, unsealed process exit"). Why the append could fail is not
/// this function's concern — a real storage fault and a plain
/// contiguity/schema violation reach the identical `Err` path, which is
/// the only property a capsule-side test can honestly claim (this
/// crate's fault harness is explicit that storage-level fault injection
/// itself is a separate, unclaimed follow-up — see `tests/fault_kill.rs`'s
/// own doc).
pub(super) fn commit_run_end_marker(
    ctx: &mut FrameCtx,
    w: &mut SegmentWriter,
    frames_written: &mut u64,
    run_end_latched: &mut bool,
    reason: String,
) -> Result<()> {
    if !*run_end_latched {
        let f = ctx.capsule_frame(
            Class::Lifecycle,
            json!({"kind": "run_end_requested", "reason": reason}),
        );
        w.append(&f, Commit::Immediate)?;
        *frames_written += 1;
        *run_end_latched = true;
    }
    Ok(())
}

/// Executes the ADR 0039 input WAL for one wire `input` frame, using the
/// store's dedupe index (folded once at open, kept live here) to fold a
/// duplicate `idem_key` per the lattice exactly, and returns the outcome
/// for the caller to report back via [`AttachProto::input_outcome`]. See
/// [`AttachAction::ForwardInput`]'s doc for the full sequence this
/// implements; `connection_authorized` is `attach_proto`'s connection-scoped
/// half of the ADR's "the capability AND the durable holder/epoch" check —
/// the durable half (against `ctx`'s own state) is this function's job, and
/// both must pass for a fresh forward.
///
/// **Finding 1 (verifier-red frames):** every controller-actor envelope
/// this function writes — `input`, `refused_stale_epoch`, `forward_intent`,
/// `forwarded` alike — carries `ctx.take_epoch`, the CURRENTLY COMMITTED
/// epoch, in its `Actor.take_epoch` field, NEVER the wire-claimed
/// (possibly stale) `take_epoch` parameter. `store/verify/` requires every
/// controller frame's declared `take_epoch` to equal whatever is committed
/// AT THAT POINT in the frame stream (`controller take_epoch {te} !=
/// committed {committed_take_epoch}` is its exact check) — that field
/// records WHEN a frame was written, not what a client claimed. Staleness
/// lives ONLY in the `refused_stale_epoch` fact itself (the fact-kind IS
/// the record that this input was rejected) plus an informational
/// `claimed` object in its body (ignorable extra JSON — ADR 0039: "unknown
/// object members are ignorable") for operators who want to see what was
/// actually asserted. `controller_id` stays the WIRE-CLAIMED identity
/// (unlike `take_epoch`, `store/verify/` never checks it against anything, and
/// recording who actually attempted the write is more useful than
/// overwriting it with the current holder's name on a REFUSED attempt).
///
/// **Finding 2 (the last-moment recheck's position):** `is_fresh` is
/// computed ONCE, here, before `input` is even committed — and that single
/// computation is what "immediately before the PTY write" (ADR 0041) means
/// in a SINGLE-THREADED, ordered writer loop: this whole function executes
/// as one uninterrupted step of that loop (no other connection's action,
/// no tick, nothing else touches `ctx.holder`/`ctx.take_epoch` between here
/// and `writer.write_all` below), so re-evaluating the same durable state
/// again right before the syscall could only ever reproduce the SAME
/// answer — which the `debug_assert!` beside the syscall states as an
/// explicit invariant rather than leaving implicit. Checking any LATER
/// than here would also break the lattice: the legal refused chain is
/// `{input, refused}` (ADR 0039 lists no `{input, intent, refused}`
/// member), so staleness must be decided BEFORE `forward_intent` is ever
/// committed, not after.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_input_wal(
    ctx: &mut FrameCtx,
    w: &mut SegmentWriter,
    store: &mut VoyageStore,
    writer: &mut dyn Write,
    frames_written: &mut u64,
    controller_id: &str,
    take_epoch: u64,
    idem_key: [u8; 16],
    payload: &[u8],
    connection_authorized: bool,
) -> Result<InputOutcome> {
    let is_fresh =
        connection_authorized && ctx.holder.as_deref() == Some(controller_id) && take_epoch == ctx.take_epoch;

    // A brand-new idem_key appends `input` first, always. The fsync of the
    // `refused_stale_epoch` or `forward_intent` fact appended next covers it
    // before any reply or the forward syscall, so "input is durably logged
    // before the producer sees it" (ADR 0039) holds either way.
    let existing = store.dedupe_index.get(&idem_key).copied();
    let input_seq = match existing {
        None => {
            let input = ctx.controller_frame(
                Class::Input,
                controller_id.to_string(),
                ctx.take_epoch,
                json!({"idem_key": hex_idem_key(&idem_key), "content": "redacted", "length": payload.len()}),
            );
            let input_seq = input.seq;
            w.append(&input, Commit::Buffered)?;
            *frames_written += 1;
            store.dedupe_index.insert(
                idem_key,
                DedupeEntry {
                    input: input_seq,
                    state: DedupeState::Input,
                    intent: None,
                },
            );
            input_seq
        }
        Some(DedupeEntry { state: DedupeState::Input, input, .. }) => {
            // Chain = {input}: a same-key retry MUST re-attempt (new
            // intent, SAME input identity) -- ADR 0039's deterministic
            // retry-fold, never a new `input` frame.
            input
        }
        Some(DedupeEntry { state: DedupeState::Intent, .. }) => return Ok(InputOutcome::DeliveryUnknown),
        Some(DedupeEntry { state: DedupeState::Forwarded, .. }) => return Ok(InputOutcome::Recorded),
        Some(DedupeEntry { state: DedupeState::Refused, .. }) => return Ok(InputOutcome::RefusedStale),
    };

    if !is_fresh {
        let mut refused = ctx.controller_frame(
            Class::Lifecycle,
            controller_id.to_string(),
            ctx.take_epoch,
            json!({"kind": "input_fact",
                   "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n}, "fact": "refused_stale_epoch"},
                   "claimed": {"controller_id": controller_id, "take_epoch": take_epoch}}),
        );
        refused.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
        w.append(&refused, Commit::Immediate)?;
        *frames_written += 1;
        if let Some(e) = store.dedupe_index.get_mut(&idem_key) {
            e.state = DedupeState::Refused;
        }
        return Ok(InputOutcome::RefusedStale);
    }

    let mut intent = ctx.controller_frame(
        Class::Lifecycle,
        controller_id.to_string(),
        ctx.take_epoch,
        json!({"kind": "input_fact",
               "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n}, "fact": "forward_intent"}}),
    );
    intent.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
    let intent_seq = intent.seq;
    w.append(&intent, Commit::Immediate)?;
    *frames_written += 1;
    if let Some(e) = store.dedupe_index.get_mut(&idem_key) {
        e.state = DedupeState::Intent;
        e.intent = Some(intent_seq);
    }

    // The "immediately before the PTY write" recheck: stated as
    // an assertion, not a second decision branch -- see this function's own
    // doc for why a DIFFERENT answer here is impossible in this
    // single-threaded loop, and why the lattice forbids acting as if it
    // could be (there is no legal `{input, intent, refused}` chain to fall
    // back to).
    debug_assert!(
        connection_authorized && ctx.holder.as_deref() == Some(controller_id) && take_epoch == ctx.take_epoch,
        "durable state changed within one WAL step -- the single-threaded writer-loop invariant was violated"
    );
    writer.write_all(payload)?; // the forward syscall

    let mut fwd = ctx.controller_frame(
        Class::Lifecycle,
        controller_id.to_string(),
        ctx.take_epoch,
        json!({"kind": "input_fact",
               "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n}, "fact": "forwarded",
                        "intent": {"epoch": intent_seq.epoch, "n": intent_seq.n}}}),
    );
    fwd.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
    // Buffered: the next commit of this segment covers it (ADR 0039 Durability invariants; flush_output).
    w.append(&fwd, Commit::Buffered)?;
    *frames_written += 1;
    if let Some(e) = store.dedupe_index.get_mut(&idem_key) {
        e.state = DedupeState::Forwarded;
    }

    Ok(InputOutcome::Recorded)
}

/// Minimal base64 (standard alphabet, padded) — duplicated from
/// `capsule/`; see the module doc.
pub(super) mod base64_engine {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    pub fn encode_b64(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
        }
        out
    }
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod tests {
    use super::*;

    #[test]
    fn wall_ms_is_unix_epoch_milliseconds_now() {
        let ms = |t: SystemTime| t.duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
        let before = ms(SystemTime::now());
        let got = wall_ms();
        let after = ms(SystemTime::now());
        assert!(before <= got && got <= after, "{before} <= {got} <= {after}");
    }

    // -- commit_run_end_marker: ADR 0041 EndRun steps 1-2, proven directly
    // against a plain SegmentWriter/FrameCtx pair -- no real ConPTY run
    // needed for the one property this function owns (the lane operation
    // semantics -- `failed {record_append}`, the hold release, the leg
    // replacement -- are U2's).

    fn run_end_marker_writer(dir: &std::path::Path, name: &str) -> (VoyageStore, SegmentWriter) {
        let root = dir.join(name);
        VoyageStore::bootstrap(&root, name, RetentionClass::Discard).unwrap();
        let mut store = VoyageStore::open_for_writing(&root, name).unwrap();
        let w = store
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        (store, w)
    }

    fn run_end_marker_ctx() -> FrameCtx {
        FrameCtx {
            epoch: 1,
            next_n: 1,
            t0: Instant::now(),
            take_epoch: 0,
            holder: None,
            attached: None,
        }
    }

    #[test]
    fn commit_run_end_marker_appends_and_latches_on_first_call() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, mut w) = run_end_marker_writer(dir.path(), "rem1");
        let mut ctx = run_end_marker_ctx();
        let mut frames_written = 0u64;
        let mut latched = false;
        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut latched, "quit".into())
            .unwrap();
        assert!(latched);
        assert_eq!(frames_written, 1);
    }

    /// Step 4: concurrent requests -- the first commit wins and writes the
    /// only marker; a later one is a no-op (its own ack still ships, at
    /// the call site, regardless of what this function does).
    #[test]
    fn commit_run_end_marker_second_call_writes_no_second_marker() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, mut w) = run_end_marker_writer(dir.path(), "rem2");
        let mut ctx = run_end_marker_ctx();
        let mut frames_written = 0u64;
        let mut latched = false;
        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut latched, "first".into())
            .unwrap();
        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut latched, "second".into())
            .unwrap();
        assert_eq!(frames_written, 1, "a second concurrent request must write no second marker");
    }

    /// A failed append (forced here via a contiguity violation — the SAME
    /// `?`-propagation path a real storage fault reaches; see
    /// `commit_run_end_marker`'s own doc for why the CAUSE of the failure
    /// is not this function's concern) leaves the latch false and
    /// propagates the error, exactly ADR 0039's crash shape: no marker,
    /// no latch — and, at the real call site, the ack action sitting
    /// after this one in the same batch is never reached either, since
    /// `run` returns before continuing the action queue.
    #[test]
    fn commit_run_end_marker_failed_append_leaves_the_latch_false_and_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, mut w) = run_end_marker_writer(dir.path(), "rem3");
        let mut ctx = run_end_marker_ctx();
        // Test-bug fix (CI: windows-latest caught this; the code path is
        // portable, not platform-divergent — see the commit message). A
        // FRESH `SegmentWriter`'s `last_seq` is `None`, and `append`'s own
        // contiguity check only fires once a PRIOR frame exists (see
        // `SegmentWriter::append`): `ctx.next_n` alone cannot violate
        // contiguity on the very first frame, so the original version of
        // this test never actually reached the error path it claimed to
        // test — it happened to type-check (this module only compiles on
        // Windows) but was never RUN until real Windows CI executed it.
        // Seed one real frame directly via `w.append` (NOT
        // `commit_run_end_marker`, which would latch and short-circuit the
        // second call this test actually exercises) so the writer has a
        // real last_seq (n=1) to violate.
        let seed = ctx.capsule_frame(Class::Lifecycle, json!({"kind": "producer_ready"}));
        w.append(&seed, Commit::Immediate).unwrap();
        ctx.next_n = 5; // breaks contiguity: the segment's last n is 1, expects 2 next
        let mut frames_written = 0u64;
        let mut latched = false;
        let err =
            commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut latched, "quit".into())
                .unwrap_err();
        assert!(format!("{err}").contains("non-contiguous"), "got: {err}");
        assert!(!latched, "a failed append must never latch");
        assert_eq!(frames_written, 0);
    }

    /// a post-write, fsync-reported
    /// failure (the write itself already durable — see
    /// `SegmentWriter::inject_fault_on_next_append_sync`'s own doc) must
    /// still propagate as a failure to THIS caller (no latch, ADR 0039's
    /// crash shape from `commit_run_end_marker`'s own perspective) —
    /// but, per ADR 0041's one-fact-one-barrier rule, the marker is
    /// ALREADY visible on disk regardless, and the typed accessor a
    /// later unit's respawn decision reads
    /// (`verify::leg_carries_run_end_marker`) must report it as present.
    /// A requester's pessimistic report can never make a real marker
    /// disappear; treating the visible byte as authoritative is what
    /// keeps "one fact, one barrier" true even when the ONE frame commit
    /// step itself is split into a write and a separate fsync outcome.
    #[test]
    fn fsync_failure_after_a_durable_write_still_leaves_the_marker_visible() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, mut w) = run_end_marker_writer(dir.path(), "rem4");
        let mut ctx = run_end_marker_ctx();
        let mut frames_written = 0u64;
        let mut latched = false;
        w.inject_fault_on_next_append_sync();
        let err = commit_run_end_marker(
            &mut ctx,
            &mut w,
            &mut frames_written,
            &mut latched,
            "quit".into(),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("injected fsync failure"), "got: {err}");
        // The caller's own view: no latch, no bookkeeping credit -- ADR
        // 0039's crash shape from this function's own perspective.
        assert!(!latched);
        assert_eq!(frames_written, 0);

        // The world's view: the bytes are genuinely on disk. Drop the
        // writer (releases the file, no further writes) and read the
        // STILL-OPEN segment fresh, exactly as a crashed leg's
        // reconciliation/accessor would.
        drop(w);
        let seg_dir = dir.path().join("rem4").join("seg");
        assert!(
            crate::store::verify::leg_carries_run_end_marker(&seg_dir, "rem4", 1).unwrap(),
            "a marker whose write succeeded (only its fsync report lied) must still be visible \
             to the accessor -- a requester's pessimistic report can never erase a real byte"
        );
    }

    /// The pty side of the input WAL: records the file length at the first
    /// write, so a test can compare it to what a sync covers by then.
    struct PtyWitness {
        open: PathBuf,
        len_at_write: Option<u64>,
        bytes: Vec<u8>,
    }

    impl PtyWitness {
        fn new(open: PathBuf) -> Self {
            Self { open, len_at_write: None, bytes: Vec::new() }
        }
    }

    impl Write for PtyWitness {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.len_at_write.is_none() {
                self.len_at_write = Some(std::fs::metadata(&self.open)?.len());
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn input_wal_ctx(epoch: u64) -> FrameCtx {
        FrameCtx {
            epoch,
            next_n: 1,
            t0: Instant::now(),
            take_epoch: 1,
            holder: Some("c1".into()),
            attached: None,
        }
    }

    /// An acked key (`Recorded`) replays as `Recorded` after a process crash
    /// and as `DeliveryUnknown` after a power loss that takes the unsynced
    /// `forwarded`; never as a second forward (ADR 0039 Durability
    /// invariants).
    #[test]
    fn input_wal_acked_key_replays_recorded_or_unknown_never_forwarded_twice() {
        const K: [u8; 16] = [7; 16];
        for power_loss in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let name = if power_loss { "iwb" } else { "iwa" };
            let (mut store, mut w) = run_end_marker_writer(dir.path(), name);
            let seg_dir = dir.path().join(name).join("seg");
            let open = w.identity().path(&seg_dir, crate::store::segment::SegmentState::Open);
            let mut ctx = input_wal_ctx(1);
            let mut witness = PtyWitness::new(open.clone());
            let mut fw = 0u64;
            let out = run_input_wal(&mut ctx, &mut w, &mut store, &mut witness, &mut fw, "c1", 1, K, b"x", true)
                .unwrap();
            assert!(matches!(out, InputOutcome::Recorded));
            let synced = w.synced_len();
            assert!(
                std::fs::metadata(&open).unwrap().len() > synced,
                "forwarded is written but not yet covered by a sync"
            );
            assert_eq!(witness.len_at_write, Some(synced), "input and intent are synced before the forward");
            drop(w);
            drop(store);
            if power_loss {
                std::fs::OpenOptions::new().write(true).open(&open).unwrap().set_len(synced).unwrap();
            }
            let mut store = VoyageStore::open_for_writing(&dir.path().join(name), name).unwrap();
            let want = if power_loss { DedupeState::Intent } else { DedupeState::Forwarded };
            assert_eq!(store.dedupe_index[&K].state, want);
            store.seal_survivor().unwrap();
            let mut w = store
                .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
                .unwrap();
            let open2 = w.identity().path(&seg_dir, crate::store::segment::SegmentState::Open);
            let mut ctx = input_wal_ctx(2);
            let mut witness = PtyWitness::new(open2);
            let mut fw = 0u64;
            let out = run_input_wal(&mut ctx, &mut w, &mut store, &mut witness, &mut fw, "c1", 1, K, b"x", true)
                .unwrap();
            if power_loss {
                assert!(matches!(out, InputOutcome::DeliveryUnknown));
            } else {
                assert!(matches!(out, InputOutcome::Recorded));
            }
            assert!(witness.bytes.is_empty(), "a replayed key is never forwarded again");
            assert_eq!(fw, 0);
        }
    }

    /// A failed fsync after an acked key halts before the next forward; a stale-epoch refusal is fsynced before its reply.
    #[test]
    fn input_wal_fsync_error_after_an_ack_halts_before_the_next_forward() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, mut w) = run_end_marker_writer(dir.path(), "iwf");
        let open = w.identity().path(&dir.path().join("iwf").join("seg"), crate::store::segment::SegmentState::Open);
        let mut ctx = input_wal_ctx(1);
        let mut witness = PtyWitness::new(open);
        let mut fw = 0u64;
        let (k1, k2) = ([1u8; 16], [2u8; 16]);
        let out = run_input_wal(&mut ctx, &mut w, &mut store, &mut witness, &mut fw, "c1", 1, k1, b"x", true)
            .unwrap();
        assert!(matches!(out, InputOutcome::Recorded));
        assert!(w.has_unsynced());
        // A stale-epoch refusal's fsync covers its `input` (and k1's `forwarded`) before the reply.
        let k3 = [3u8; 16];
        let out = run_input_wal(&mut ctx, &mut w, &mut store, &mut witness, &mut fw, "c1", 1, k3, b"z", false)
            .unwrap();
        assert!(matches!(out, InputOutcome::RefusedStale));
        assert!(!w.has_unsynced(), "a stale-epoch refusal is fsynced before its reply");
        assert_eq!(store.dedupe_index[&k3].state, DedupeState::Refused);
        w.inject_fault_on_next_append_sync();
        let err = run_input_wal(&mut ctx, &mut w, &mut store, &mut witness, &mut fw, "c1", 1, k2, b"y", true)
            .unwrap_err();
        assert!(err.to_string().contains("injected fsync failure"), "{err}");
        assert_eq!(witness.bytes, b"x");
        assert_eq!(store.dedupe_index[&k2].state, DedupeState::Input);
    }
}
