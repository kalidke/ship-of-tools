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
