use super::*;
use sot_platform::identity::challenge::{ChallengeOutcome, PeerAuthOutcome};
use sot_platform::identity::exchange::IdentityExchange;
use sot_platform::identity::peer::PeerIdentity;
use sot_platform::transport::Client;
use sot_protocol::{codec::write_frame_blocking_blob, Frame};
use std::io::Read;

struct Peer;
impl PeerIdentity for Peer {
    fn pid(&self) -> u32 {
        1
    }
    fn created(&self) -> u64 {
        1
    }
}
struct Stream {
    bytes: Mutex<std::io::Cursor<Vec<u8>>>,
    fault: &'static str,
}
impl Client for Stream {
    fn write_all(&self, _: &[u8]) -> Result<(), TransportError> {
        Ok(())
    }
    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let n = self.bytes.lock().unwrap().read(buf).unwrap();
        if n == 0 && self.fault == "reset" {
            Err(TransportError::Io {
                op: "read",
                source: std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "fixture read reset",
                ),
            })
        } else {
            Ok(n)
        }
    }
    fn cancel(&self) {}
}
struct Script {
    stage: &'static str,
    fault: &'static str,
}
impl Endpoint for Script {
    type Client = Stream;
    type Process = Peer;
    fn connect_row_unchallenged(&self, _: &str) -> Result<Stream, TransportError> {
        if self.stage == "connecting" {
            return Err(TransportError::Refused {
                code: "fixture".into(),
                detail: "fixture dial refused".into(),
            });
        }
        let mut bytes = Vec::new();
        let payload = if self.stage == "attach" {
            serde_json::json!({"error": "fixture attach refused"})
        } else {
            serde_json::json!({"run": "one"})
        };
        write_frame_blocking_blob(&mut bytes, &Frame::res(2, "row.attach", payload), None).unwrap();
        if self.stage == "service" {
            write_frame_blocking_blob(
                &mut bytes,
                &Frame::evt(
                    "row.snapshot",
                    serde_json::json!({"last": true, "blob": {"len": 0, "mime": "application/octet-stream"}}),
                ),
                Some(&[]),
            )
            .unwrap();
        }
        if self.stage == "service" && self.fault == "payload" {
            write_frame_blocking_blob(
                &mut bytes,
                &Frame::evt(
                    "row.snapshot",
                    serde_json::json!({"last": "secret=fixture-value"}),
                ),
                None,
            )
            .unwrap();
        }
        if self.stage == "service" && self.fault == "codec" {
            bytes.extend_from_slice(b"not JSON secret=fixture-value\n");
        }
        Ok(Stream {
            bytes: Mutex::new(std::io::Cursor::new(bytes)),
            fault: self.fault,
        })
    }
    fn challenge(
        &self,
        _: &Stream,
        _: &mut dyn IdentityExchange,
        _: Instant,
    ) -> ChallengeOutcome<Peer> {
        if self.stage == "hello" {
            ChallengeOutcome::Undetermined
        } else {
            ChallengeOutcome::Proven(Peer)
        }
    }
    fn authenticate_server(&self, _: &Stream) -> PeerAuthOutcome {
        unreachable!()
    }
}

fn drive(stage: &'static str, purposeful: bool) -> (String, Vec<String>) {
    drive_fault(stage, purposeful, "reset")
}

fn drive_fault(
    stage: &'static str,
    purposeful: bool,
    fault: &'static str,
) -> (String, Vec<String>) {
    drive_as(stage, purposeful, fault, false)
}

fn drive_as(
    stage: &'static str,
    purposeful: bool,
    fault: &'static str,
    headless: bool,
) -> (String, Vec<String>) {
    let log = sot_platform::test_log::capture();
    let mut state = Shared::new(80, 24, true);
    state.host = "example-host".into();
    let shared = Arc::new(state);
    if purposeful {
        *shared.shutdown_reason.lock().unwrap() = "switch away";
    }
    let statuses = Mutex::new(Vec::new());
    let (_tx, inbox) = mpsc::channel();
    let sink = |event| match event {
        WorkerEvent::Checkpoint { .. } if purposeful => {
            shared.shutdown.store(true, Ordering::Release);
        }
        WorkerEvent::Status(text) if text != "attached" => {
            statuses.lock().unwrap().push(text);
            shared.shutdown.store(true, Ordering::Release);
        }
        WorkerEvent::Terminal(text) => {
            statuses.lock().unwrap().push(text);
        }
        _ => {}
    };
    Worker {
        endpoint: Script { stage, fault },
        row: "example-row".into(),
        headless,
        shared: shared.clone(),
        inbox,
        sink,
    }
    .run();
    (log.text(), statuses.into_inner().unwrap())
}

#[test]
fn every_attach_end_names_its_stage_and_intent_once() {
    for stage in ["connecting", "hello", "attach", "snapshot", "service"] {
        let (log, _) = drive(stage, false);
        let ends: Vec<_> = log
            .lines()
            .filter(|l| l.contains("row attach ended"))
            .collect();
        assert_eq!(ends.len(), 1, "{stage}: {log}");
        for field in [
            "host=\"example-host\"",
            "row=\"example-row\"",
            "attempt=1",
            &format!("stage={stage}"),
            "end=lost",
            "elapsed_since_attempt_start_ms=",
            "error=",
        ] {
            assert!(ends[0].contains(field), "missing {field}: {log}");
        }
    }
    let (log, _) = drive("service", true);
    assert_eq!(
        log.lines()
            .filter(|l| l.contains("row attach ended"))
            .count(),
        1,
        "{log}"
    );
    assert!(
        log.contains("end=shutdown") && log.contains("switch away"),
        "{log}"
    );
}

#[test]
fn a_headless_client_records_only_a_loss() {
    let ends = |log: &str| {
        log.lines()
            .filter(|l| l.contains("row attach ended"))
            .count()
    };
    let (log, _) = drive_as("service", true, "reset", true);
    assert_eq!(ends(&log), 0, "{log}");
    let (log, _) = drive_as("service", false, "reset", true);
    assert_eq!(ends(&log), 1, "{log}");
}

#[test]
fn a_service_read_error_keeps_its_cause_and_the_old_status() {
    for (fault, cause) in [
        ("reset", "read envelope: fixture read reset"),
        ("eof", "eof after 0 byte(s)"),
        ("codec", "frame parse failed: Syntax error"),
        ("payload", "protocol: Data error"),
    ] {
        let (log, statuses) = drive_fault("service", false, fault);
        assert_eq!(statuses, ["redialing…"]);
        assert!(log.contains(cause), "{log}");
        assert!(
            !log.contains("fixture-value"),
            "a malformed envelope's contents leaked: {log}"
        );
    }
}

/// Before its first attach a worker knows no run, so input has none to be typed into: it is refused and counted, not
/// queued to land in whichever run the attach finds.
#[test]
fn input_before_the_first_attach_is_refused_and_counted() {
    let (w, inbox) = AttachWorker::inert_with_inbox();
    assert!(!w.send_input(b"x".to_vec()), "input with no run identity was queued");
    assert!(!w.send_input_for(b"y".to_vec(), Some(String::new())), "an empty run names nothing");
    assert_eq!(w.inputs_discarded(), 2);
    assert_eq!(w.unsent_inputs(), 0);
    assert!(inbox.try_recv().is_err(), "a refused input reached the worker's queue");
}

/// Input the worker has not yet taken is counted, so a caller that retires the client can say what was lost (the
/// count falls as the worker takes input: `fe_attach::a_demoted_window_types_again_after_a_wake`).
#[test]
fn input_waiting_for_the_worker_is_counted_as_unsent() {
    let (w, inbox) = AttachWorker::inert_with_inbox();
    *w.shared.run.lock().unwrap() = "one".into();
    assert!(w.send_input(b"a".to_vec()));
    assert!(w.send_input_for(b"bc".to_vec(), Some("one".into())));
    assert_eq!(w.unsent_inputs(), 2);
    assert_eq!(w.inputs_discarded(), 0);
    drop(inbox);
}
