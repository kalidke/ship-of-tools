use super::*;

#[test]
fn epoch_ms_is_unix_epoch_milliseconds() {
    let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
    assert_eq!(epoch_ms(t), 1_700_000_000_123);
    assert_eq!(epoch_ms(UNIX_EPOCH - Duration::from_secs(1)), 0, "a time before the epoch reads 0");
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
