//! The blocking row client: one connection, a reader thread, requests answered in order.

use super::proto::{
    AttachReq, AttachRes, InputRes, ResizeReq, RowError, RowHelloExchange, SnapshotEvt, OP_ATTACH, OP_DEMOTED, OP_INPUT,
    OP_OUTPUT, OP_RESIZE, OP_SNAPSHOT, OP_TAKE,
};
use super::ROW_PROTOCOL;
use crate::lane::client::{Endpoint, PlatformEndpoint};
use crate::lane::connect_own::connect_own;
use sot_platform::identity::challenge::ChallengeOutcome;
use sot_platform::identity::deadline::run_with_deadline;
use sot_protocol::codec::{read_frame_blocking_blob, write_frame_blocking_blob};
use sot_protocol::{Frame, Kind};
use std::collections::VecDeque;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a request waits for its reply.
const REQUEST_BOUND: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowEvent {
    /// A piece of the snapshot; `last` ends it.
    Snapshot { bytes: Vec<u8>, last: bool },
    Output(Vec<u8>),
    /// Another connection took the driver's place.
    Demoted,
}

/// Frames the reader thread holds for the caller. Past it the reader stops reading the socket, so a caller that stops
/// pumping pushes back on the row, which then drops it as a slow viewer.
const INCOMING_FRAMES: usize = 256;
/// Events kept while a request waits for its reply, at most.
const PENDING_EVENTS: usize = 1024;

type Incoming = Result<(Frame, Option<Vec<u8>>), RowError>;

type Conn = <PlatformEndpoint as Endpoint>::Client;

/// `Read` over the lane client's own blocking read.
struct ConnReader(Arc<Conn>);

impl Read for ConnReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf).map_err(crate::lane::client::transport_error_to_io)
    }
}

pub struct RowClient {
    conn: Arc<Conn>,
    incoming: mpsc::Receiver<Incoming>,
    events: VecDeque<RowEvent>,
    next_id: u64,
    run: String,
}

impl RowClient {
    /// Dials, proves the peer and checks its row number, all within `bound`. A row of another row number is refused by
    /// name; the frames' `v` is never read.
    pub fn connect(path: &Path, build: &str, bound: Duration) -> Result<Self, RowError> {
        let deadline = Instant::now() + bound;
        // The platform connector keeps its own 2 s retry budget and cannot be cut short; a dial that outlasts the
        // caller's deadline is reported as a timeout when it returns.
        let conn = connect_own(path).map_err(transport)?;
        if Instant::now() >= deadline {
            return Err(RowError::Timeout("the dial"));
        }
        let mut hello = RowHelloExchange::new(build);
        match PlatformEndpoint::default().challenge(&conn, &mut hello, deadline) {
            ChallengeOutcome::Proven(_) => {}
            ChallengeOutcome::Foreign => {
                return Err(match hello.mismatch() {
                    Some(theirs) => RowError::RowMismatch { ours: ROW_PROTOCOL, theirs },
                    None => RowError::Foreign,
                })
            }
            // A deadline that passed is a timeout; any other failure to prove (an end of stream, an OS error, a start in
            // the clock tick of the connect on Linux) is that the peer could not be proven.
            ChallengeOutcome::Undetermined if Instant::now() >= deadline => return Err(RowError::Timeout("the row's hello")),
            ChallengeOutcome::Undetermined => return Err(RowError::Unproven),
        }
        let run = hello.reply().map(|r| r.run.clone()).unwrap_or_default();
        let conn = Arc::new(conn);
        let (tx, incoming) = mpsc::sync_channel(INCOMING_FRAMES);
        let reader = ConnReader(Arc::clone(&conn));
        std::thread::Builder::new().name("row-client-read".into()).spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                match read_frame_blocking_blob(&mut reader) {
                    Ok(frame) => {
                        if tx.send(Ok(frame)).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(Err(RowError::Closed));
                        return;
                    }
                }
            }
        })?;
        Ok(Self { conn, incoming, events: VecDeque::new(), next_id: 2, run })
    }

    /// The run the row declared in its hello.
    pub fn run(&self) -> &str {
        &self.run
    }

    /// Attaches; the snapshot and then the output arrive as events. As a driver the row is resized to this window.
    pub fn attach(&mut self, cols: u16, rows: u16, driver: bool) -> Result<String, RowError> {
        let req = serde_json::to_value(AttachReq { cols, rows, driver }).unwrap_or_default();
        let res = self.request(OP_ATTACH, req, None)?;
        let res: AttachRes = serde_json::from_value(res.payload).map_err(|e| RowError::Protocol(e.to_string()))?;
        self.run.clone_from(&res.run);
        Ok(res.run)
    }

    /// The next event, or `None` when none arrives within `wait`.
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<RowEvent>, RowError> {
        if let Some(ev) = self.events.pop_front() {
            return Ok(Some(ev));
        }
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.incoming.recv_timeout(left) {
                Ok(Ok((frame, blob))) => {
                    if let Some(ev) = event_of(frame, blob)? {
                        return Ok(Some(ev));
                    }
                }
                Ok(Err(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(RowError::Closed),
            }
        }
    }

    /// Becomes the driver; the one before is demoted.
    pub fn take(&mut self) -> Result<(), RowError> {
        self.request(OP_TAKE, serde_json::json!({}), None).map(|_| ())
    }

    /// Types. Refused by name unless this connection is the driver; the count is what the terminal took.
    pub fn send_input(&mut self, bytes: &[u8]) -> Result<usize, RowError> {
        let payload = serde_json::json!({"blob": {"len": bytes.len(), "mime": "application/octet-stream"}});
        let res = self.request(OP_INPUT, payload, Some(bytes))?;
        let res: InputRes = serde_json::from_value(res.payload).map_err(|e| RowError::Protocol(e.to_string()))?;
        Ok(res.written as usize)
    }

    /// Resizes the terminal; the driver only.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), RowError> {
        let req = serde_json::to_value(ResizeReq { cols, rows }).unwrap_or_default();
        self.request(OP_RESIZE, req, None).map(|_| ())
    }

    fn request(&mut self, op: &str, payload: serde_json::Value, blob: Option<&[u8]>) -> Result<Frame, RowError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut bytes = Vec::new();
        write_frame_blocking_blob(&mut bytes, &Frame::req(id, op, payload), blob).map_err(|e| RowError::Protocol(e.to_string()))?;
        let deadline = Instant::now() + REQUEST_BOUND;
        // The write is under the request's own deadline: a row that stops reading cannot park the caller.
        let conn = Arc::clone(&self.conn);
        match run_with_deadline(deadline, || self.conn.cancel(), move || conn.write_all(&bytes)) {
            Some(written) => written.map_err(transport)?,
            None => return Err(RowError::Timeout("a row request's write")),
        }
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.incoming.recv_timeout(left) {
                Ok(Ok((frame, blob))) => match frame.kind {
                    Kind::Res if frame.id == id => {
                        if let Some(msg) = frame.payload.get("error").and_then(|e| e.as_str()) {
                            return Err(match frame.payload.get("code").and_then(|c| c.as_str()) {
                                Some("unknown") => RowError::DeliveryUnknown,
                                _ => RowError::Refused(msg.to_string()),
                            });
                        }
                        return Ok(frame);
                    }
                    Kind::Evt => {
                        if let Some(ev) = event_of(frame, blob)? {
                            if self.events.len() >= PENDING_EVENTS {
                                return Err(RowError::Protocol("too many events were left unread".into()));
                            }
                            self.events.push_back(ev);
                        }
                    }
                    _ => {}
                },
                Ok(Err(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(RowError::Timeout("a row request")),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(RowError::Closed),
            }
        }
    }
}

impl Drop for RowClient {
    fn drop(&mut self) {
        self.conn.cancel();
    }
}

fn transport(e: crate::lane::transport::TransportError) -> RowError {
    RowError::Io(crate::lane::client::transport_error_to_io(e))
}

/// A frame as an event; a reply with no request waiting is not one.
fn event_of(frame: Frame, blob: Option<Vec<u8>>) -> Result<Option<RowEvent>, RowError> {
    if frame.kind != Kind::Evt {
        return Ok(None);
    }
    Ok(match frame.op.as_str() {
        OP_SNAPSHOT => {
            let evt: SnapshotEvt = serde_json::from_value(frame.payload).map_err(|e| RowError::Protocol(e.to_string()))?;
            Some(RowEvent::Snapshot { bytes: blob.unwrap_or_default(), last: evt.last })
        }
        OP_OUTPUT => Some(RowEvent::Output(blob.unwrap_or_default())),
        OP_DEMOTED => Some(RowEvent::Demoted),
        _ => None,
    })
}
