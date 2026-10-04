//! Tests for `FeAttachClient`: pump and checkpoint bookkeeping over a hand-built worker.
use super::*;

/// LU6a: a fresh client reports `is_checkpointed() == false`, and
/// `true` once a `Checkpoint` event has gone through `pump` -- the
/// SAME arm a real worker's checkpoint event drains through. Built by
/// hand rather than via `attach` (which spawns a real worker thread
/// and needs a real state dir/lane): this module's own child-module
/// privacy lets the struct literal reach every private field, and
/// `pump` neither knows nor cares whether `events_tx` belongs to a
/// worker thread or a test.
// `PlatformEndpoint` only exists on Windows, Linux and macOS.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn checkpoint_event_marks_the_client_checkpointed() {
    let (events_tx, events_rx) = mpsc::channel();
    let mut client = FeAttachClient::<PlatformEndpoint> {
        parser: vt100_ctt::Parser::new(24, 80, 100),
        pane_size: (24, 80),
        worker: AttachWorker::stub_for_test(),
        events_rx,
        status: "connecting\u{2026}".to_string(),
        notice: None,
        quit_message: None,
        should_exit: false,
        dead: false,
        checkpointed: false,
        restore_ok: false,
        pending_fe_down_markers: VecDeque::new(),
        headless: false,
        recorded_bytes: Arc::new(AtomicU64::new(0)),
        take_epoch: Arc::new(AtomicU64::new(0)),
        last_input_outcome: Arc::new(Mutex::new(None)),
    };
    assert!(!client.is_checkpointed(), "a fresh client must not report checkpointed");
    assert!(!client.restore_ok(), "a fresh client must not report a successful restore");

    let bytes = vt100_ctt::Parser::new(24, 80, 100)
        .screen()
        .checkpoint()
        .expect("encode a checkpoint of a fresh, in-range screen");
    events_tx.send(WorkerEvent::Checkpoint(bytes)).expect("send a synthetic checkpoint event");
    client.pump();
    assert!(
        client.is_checkpointed(),
        "pump()'s Checkpoint arm must mark the client checkpointed"
    );
    assert!(client.restore_ok(), "a well-formed checkpoint must restore successfully");
}

/// switch-latency Phase 1: `restore_ok` must go `false`, even though
/// `is_checkpointed` still goes `true` (LU6a's own "the checkpoint
/// EVENT landed either way") -- the exact gap `restore_ok` exists to
/// close for a caller's instrumentation.
// `PlatformEndpoint` only exists on Windows, Linux and macOS.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn checkpoint_event_with_undecodable_bytes_marks_checkpointed_but_not_restore_ok() {
    let (events_tx, events_rx) = mpsc::channel();
    let mut client = FeAttachClient::<PlatformEndpoint> {
        parser: vt100_ctt::Parser::new(24, 80, 100),
        pane_size: (24, 80),
        worker: AttachWorker::stub_for_test(),
        events_rx,
        status: "connecting\u{2026}".to_string(),
        notice: None,
        quit_message: None,
        should_exit: false,
        dead: false,
        checkpointed: false,
        restore_ok: false,
        pending_fe_down_markers: VecDeque::new(),
        headless: false,
        recorded_bytes: Arc::new(AtomicU64::new(0)),
        take_epoch: Arc::new(AtomicU64::new(0)),
        last_input_outcome: Arc::new(Mutex::new(None)),
    };

    events_tx
        .send(WorkerEvent::Checkpoint(vec![0xff; 4]))
        .expect("send a synthetic, undecodable checkpoint event");
    client.pump();
    assert!(client.is_checkpointed(), "the checkpoint EVENT still landed");
    assert!(!client.restore_ok(), "a failed restore must not report restore_ok");
}
/// Codex review round finding 2: a fresh checkpoint (a new attach
/// episode, possibly against a DIFFERENT leg after a reconnect) must
/// retract whatever notice was showing for the PREVIOUS leg, rather
/// than leaving it standing until a new `Notice` event replaces it.
/// The attach notice is emitted only after the checkpoint (see
/// `run_worker`'s own reorder comment), so without this clear, a
/// caller reading `notice()` right after this `pump()` call -- before
/// the worker's own mgmt-lane lookup for the NEW leg has produced its
/// own `Notice` -- would see leg A's stale text rendered over leg B's
/// freshly restored screen.
// `PlatformEndpoint` only exists on Windows, Linux and macOS.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn checkpoint_event_clears_a_notice_left_over_from_the_previous_leg() {
    let (events_tx, events_rx) = mpsc::channel();
    let mut client = FeAttachClient::<PlatformEndpoint> {
        parser: vt100_ctt::Parser::new(24, 80, 100),
        pane_size: (24, 80),
        worker: AttachWorker::stub_for_test(),
        events_rx,
        status: "connecting\u{2026}".to_string(),
        notice: None,
        quit_message: None,
        should_exit: false,
        dead: false,
        checkpointed: false,
        restore_ok: false,
        pending_fe_down_markers: VecDeque::new(),
        headless: false,
        recorded_bytes: Arc::new(AtomicU64::new(0)),
        take_epoch: Arc::new(AtomicU64::new(0)),
        last_input_outcome: Arc::new(Mutex::new(None)),
    };

    events_tx
        .send(WorkerEvent::Notice("leg A started at ...".to_string()))
        .expect("send a synthetic notice for leg A");
    client.pump();
    assert_eq!(client.notice(), Some("leg A started at ..."));

    let bytes = vt100_ctt::Parser::new(24, 80, 100)
        .screen()
        .checkpoint()
        .expect("encode a checkpoint of a fresh, in-range screen");
    events_tx
        .send(WorkerEvent::Checkpoint(bytes))
        .expect("send leg B's own checkpoint -- no Notice for leg B has arrived yet");
    client.pump();
    assert_eq!(
        client.notice(),
        None,
        "leg A's notice must be retracted the moment leg B's checkpoint lands, not left \
         standing until leg B's own Notice (if any) arrives"
    );
}

/// Successive checkpoints (each attach episode gets its own) must
/// each report `restore_ok` for THEIR OWN restore, not a value stuck
/// from an earlier one -- proven here across three in a row:
/// success, failure, success again.
// `PlatformEndpoint` only exists on Windows, Linux and macOS.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn restore_ok_reflects_only_the_most_recent_checkpoint_across_several_in_a_row() {
    let (events_tx, events_rx) = mpsc::channel();
    let mut client = FeAttachClient::<PlatformEndpoint> {
        parser: vt100_ctt::Parser::new(24, 80, 100),
        pane_size: (24, 80),
        worker: AttachWorker::stub_for_test(),
        events_rx,
        status: "connecting\u{2026}".to_string(),
        notice: None,
        quit_message: None,
        should_exit: false,
        dead: false,
        checkpointed: false,
        restore_ok: false,
        pending_fe_down_markers: VecDeque::new(),
        headless: false,
        recorded_bytes: Arc::new(AtomicU64::new(0)),
        take_epoch: Arc::new(AtomicU64::new(0)),
        last_input_outcome: Arc::new(Mutex::new(None)),
    };
    let good_checkpoint = || {
        vt100_ctt::Parser::new(24, 80, 100)
            .screen()
            .checkpoint()
            .expect("encode a checkpoint of a fresh, in-range screen")
    };

    events_tx.send(WorkerEvent::Checkpoint(good_checkpoint())).expect("send checkpoint 1 (good)");
    client.pump();
    assert!(client.restore_ok(), "checkpoint 1 (good) must report restore_ok");

    events_tx.send(WorkerEvent::Checkpoint(vec![0xff; 4])).expect("send checkpoint 2 (bad)");
    client.pump();
    assert!(!client.restore_ok(), "checkpoint 2 (bad) must clear restore_ok, not inherit checkpoint 1's");

    events_tx.send(WorkerEvent::Checkpoint(good_checkpoint())).expect("send checkpoint 3 (good)");
    client.pump();
    assert!(client.restore_ok(), "checkpoint 3 (good) must report restore_ok again, not inherit checkpoint 2's");
}
