//! Tests for `FeAttachClient`: pump and snapshot bookkeeping over a worker with no thread.
use super::*;

fn client(headless: bool) -> (FeAttachClient, mpsc::Sender<WorkerEvent>) {
    let (events_tx, events_rx) = mpsc::channel();
    (FeAttachClient::with_worker(AttachWorker::inert(), events_rx, 24, 80, headless), events_tx)
}

fn snapshot(rows: u16, cols: u16) -> Vec<u8> {
    vt100_ctt::Parser::new(rows, cols, 100).screen().checkpoint().expect("encode a checkpoint of a fresh, in-range screen")
}

#[test]
fn a_snapshot_marks_the_client_checkpointed() {
    let (mut client, events) = client(false);
    assert!(!client.is_checkpointed(), "a fresh client must not report checkpointed");
    events.send(WorkerEvent::Checkpoint(snapshot(24, 80))).unwrap();
    client.pump();
    assert!(client.is_checkpointed());
}

#[test]
fn a_snapshot_that_does_not_restore_marks_checkpointed_and_says_so() {
    let (mut client, events) = client(false);
    events.send(WorkerEvent::Checkpoint(vec![0xff; 4])).unwrap();
    events.send(WorkerEvent::Status("attached".into())).unwrap();
    client.pump();
    assert!(client.is_checkpointed(), "the snapshot event landed either way");
    assert!(client.status_line().starts_with("checkpoint restore failed"), "{}", client.status_line());
}

#[test]
fn a_window_reflows_a_snapshot_to_its_pane_and_a_headless_client_adopts_its_size() {
    let (mut window, events) = client(false);
    events.send(WorkerEvent::Checkpoint(snapshot(30, 100))).unwrap();
    window.pump();
    assert_eq!(window.screen().size(), (24, 80));
    let (mut headless, events) = client(true);
    events.send(WorkerEvent::Checkpoint(snapshot(30, 100))).unwrap();
    headless.pump();
    assert_eq!(headless.screen().size(), (30, 100));
}

#[test]
fn a_terminal_event_kills_the_client_and_names_why() {
    let (mut client, events) = client(false);
    events.send(WorkerEvent::Terminal("the row speaks row protocol 2".into())).unwrap();
    client.pump();
    assert!(client.is_dead());
    assert_eq!(client.status_line(), "the row speaks row protocol 2");
}

#[test]
fn output_is_drained_in_order_into_the_screen() {
    let (mut client, events) = client(false);
    events.send(WorkerEvent::Output(b"one ".to_vec())).unwrap();
    events.send(WorkerEvent::Output(b"two".to_vec())).unwrap();
    assert!(client.pump());
    assert!(client.screen().contents().starts_with("one two"), "{}", client.screen().contents());
}

/// An OSC 52 copy in the row's output is kept for the window to take, once, whole across two output events; a newer
/// one replaces an untaken one; a request to read the clipboard is never kept. A pump stopped at its cap says the
/// backlog is not drained.
#[test]
fn a_copy_in_the_output_is_taken_once_and_a_read_request_never() {
    let (mut client, events) = client(false);
    events.send(WorkerEvent::Output(b"text \x1b]52;c;aGVs".to_vec())).unwrap();
    events.send(WorkerEvent::Output(b"bG8=\x07 more".to_vec())).unwrap();
    client.pump();
    assert!(client.backlog_drained());
    assert_eq!(client.take_copy().as_deref(), Some(&b"aGVsbG8="[..]));
    assert_eq!(client.take_copy(), None, "taken once");
    events.send(WorkerEvent::Output(b"\x1b]52;c;b25l\x07\x1b]52;c;dHdv\x07\x1b]52;c;?\x07".to_vec())).unwrap();
    client.pump();
    assert_eq!(client.take_copy().as_deref(), Some(&b"dHdv"[..]), "the newer copy; the read request kept nothing");
    for _ in 0..3 {
        events.send(WorkerEvent::Output(vec![b'x'; PUMP_DRAIN_CAP_BYTES / 2])).unwrap();
    }
    client.pump();
    assert!(!client.backlog_drained(), "stopped at the cap with output still waiting");
    client.pump();
    assert!(client.backlog_drained());
}
