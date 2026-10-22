//! The run side of the row socket: what the supervisor's main loop feeds and drains.

use super::proto::{AttachRes, OP_ATTACH, OP_DEMOTED, OP_OUTPUT, OP_SNAPSHOT, OUTPUT_CHUNK, SNAPSHOT_CHUNK};
use super::MAX_CONNECTIONS;
use serde_json::json;
use sot_protocol::Frame;
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// What a connection's writer sends: a frame with its blob, or the end of the connection.
pub(super) enum Out {
    Frame(Frame, Option<Vec<u8>>),
    Close,
}

/// A request a connection hands the main loop; the loop does it and answers.
pub enum RowRequest {
    Input { bytes: Vec<u8>, reply: mpsc::SyncSender<Result<usize, String>> },
    Resize { cols: u16, rows: u16 },
}

/// How long a connection waits for the main loop to take its input.
const INPUT_BOUND: Duration = Duration::from_secs(2);
/// The frames a connection may have queued; a viewer that does not drain them is dropped.
pub(super) const QUEUE_FRAMES: usize = 256;

pub(super) type ConnId = u64;

struct Conn {
    tx: mpsc::SyncSender<Out>,
    /// The snapshot was sent: output flows to it.
    live: bool,
}

struct Inner {
    run: String,
    phase: String,
    conns: HashMap<ConnId, Conn>,
    next_conn: ConnId,
    driver: Option<ConnId>,
    /// Attached connections that wait for a parser ground state to be sent the screen.
    pending: Vec<ConnId>,
    requests: VecDeque<RowRequest>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// One run's fan-out. The main loop calls [`RowRun::output`] after the parser took a chunk, [`RowRun::tick`] every pass,
/// and drains [`RowRun::take_requests`]; connections never touch the parser or the terminal.
pub struct RowRun {
    inner: Mutex<Inner>,
}

impl RowRun {
    pub fn new(run: impl Into<String>, phase: &str) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                run: run.into(),
                phase: phase.to_string(),
                conns: HashMap::new(),
                next_conn: 1,
                driver: None,
                pending: Vec::new(),
                requests: VecDeque::new(),
                wake: None,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn run_id(&self) -> String {
        self.lock().run.clone()
    }

    pub fn phase(&self) -> String {
        self.lock().phase.clone()
    }

    pub fn set_phase(&self, phase: &str) {
        self.lock().phase = phase.to_string();
    }

    /// A new agent start is a new run: the id changes, and connections of the old run keep their pen and their output.
    pub fn set_run(&self, run: impl Into<String>) {
        self.lock().run = run.into();
    }

    /// What the loop calls to be woken when a request arrives.
    pub fn set_wake(&self, wake: Box<dyn Fn() + Send + Sync>) {
        self.lock().wake = Some(Arc::from(wake));
    }

    /// Called with the chunk the parser just took: live connections get it in order, then any waiting attach gets the
    /// screen as it is now (which already holds the chunk) and goes live behind it.
    pub fn output(&self, bytes: &[u8], parser: &vt100_ctt::Parser) {
        let mut inner = self.lock();
        let live: Vec<ConnId> = inner.conns.iter().filter(|(_, c)| c.live).map(|(id, _)| *id).collect();
        for chunk in bytes.chunks(OUTPUT_CHUNK) {
            for id in &live {
                let frame = Frame::evt(OP_OUTPUT, blob_payload(json!({}), chunk.len()));
                inner.send(*id, frame, Some(chunk.to_vec()));
            }
        }
        inner.serve_pending(parser);
    }

    /// Serves attaches waiting for a ground state.
    pub fn tick(&self, parser: &vt100_ctt::Parser) {
        let mut inner = self.lock();
        if !inner.pending.is_empty() {
            inner.serve_pending(parser);
        }
    }

    pub fn take_requests(&self) -> Vec<RowRequest> {
        self.lock().requests.drain(..).collect()
    }

    /// Ends every connection: the run is over for them.
    pub fn close_all(&self) {
        let mut inner = self.lock();
        for conn in inner.conns.values() {
            let _ = conn.tx.try_send(Out::Close);
        }
        inner.conns.clear();
        inner.pending.clear();
        inner.driver = None;
    }

    pub(super) fn open(&self) -> Option<(ConnId, mpsc::Receiver<Out>)> {
        let mut inner = self.lock();
        if inner.conns.len() >= MAX_CONNECTIONS {
            return None;
        }
        let id = inner.next_conn;
        inner.next_conn += 1;
        let (tx, rx) = mpsc::sync_channel(QUEUE_FRAMES);
        inner.conns.insert(id, Conn { tx, live: false });
        Some((id, rx))
    }

    pub(super) fn close(&self, id: ConnId) {
        let mut inner = self.lock();
        inner.conns.remove(&id);
        inner.pending.retain(|p| *p != id);
        if inner.driver == Some(id) {
            inner.driver = None;
        }
    }

    pub(super) fn send(&self, id: ConnId, frame: Frame, blob: Option<Vec<u8>>) {
        self.lock().send(id, frame, blob);
    }

    /// Admits an attach: the reply goes first, then the screen follows at the next ground state. A driver's attach
    /// demotes the driver before it and resizes the terminal to the attaching window.
    pub(super) fn attach(&self, id: ConnId, req_id: u64, cols: u16, rows: u16, driver: bool) -> Result<(), String> {
        let mut inner = self.lock();
        if driver {
            check_size(cols, rows)?;
        }
        let res = Frame::res(req_id, OP_ATTACH, serde_json::to_value(AttachRes { run: inner.run.clone() }).unwrap_or_default());
        inner.send(id, res, None);
        if !inner.pending.contains(&id) {
            inner.pending.push(id);
        }
        if let Some(conn) = inner.conns.get_mut(&id) {
            conn.live = false;
        }
        if driver {
            inner.make_driver(id);
            inner.request(RowRequest::Resize { cols, rows });
        }
        Ok(())
    }

    pub(super) fn take(&self, id: ConnId) {
        self.lock().make_driver(id);
    }

    #[cfg(test)]
    pub(super) fn is_driver(&self, id: ConnId) -> bool {
        self.lock().driver == Some(id)
    }

    /// Hands bytes to the main loop and waits for it to write them.
    pub(super) fn input(&self, id: ConnId, bytes: Vec<u8>) -> Result<usize, String> {
        let (reply, answered) = mpsc::sync_channel(1);
        {
            let mut inner = self.lock();
            if inner.driver != Some(id) {
                return Err("this connection is not the driver".into());
            }
            inner.request(RowRequest::Input { bytes, reply });
        }
        match answered.recv_timeout(INPUT_BOUND) {
            Ok(r) => r,
            Err(_) => Err("the terminal did not take the input in time".into()),
        }
    }

    pub(super) fn resize(&self, id: ConnId, cols: u16, rows: u16) -> Result<(), String> {
        check_size(cols, rows)?;
        let mut inner = self.lock();
        if inner.driver != Some(id) {
            return Err("this connection is not the driver".into());
        }
        inner.request(RowRequest::Resize { cols, rows });
        Ok(())
    }
}

fn check_size(cols: u16, rows: u16) -> Result<(), String> {
    if (2..=512).contains(&cols) && (2..=256).contains(&rows) {
        Ok(())
    } else {
        Err(format!("a terminal of {cols}x{rows} is outside 2x2..512x256"))
    }
}

/// `payload` with a `blob` descriptor of `len` bytes.
fn blob_payload(mut payload: serde_json::Value, len: usize) -> serde_json::Value {
    payload["blob"] = json!({"len": len, "mime": "application/octet-stream"});
    payload
}

impl Inner {
    /// Queues a frame for a connection; a connection whose queue is full is cut, never waited for.
    fn send(&mut self, id: ConnId, frame: Frame, blob: Option<Vec<u8>>) {
        let Some(conn) = self.conns.get(&id) else { return };
        if conn.tx.try_send(Out::Frame(frame, blob)).is_err() {
            let _ = conn.tx.try_send(Out::Close);
            self.conns.remove(&id);
            self.pending.retain(|p| *p != id);
            if self.driver == Some(id) {
                self.driver = None;
            }
        }
    }

    fn request(&mut self, req: RowRequest) {
        self.requests.push_back(req);
        if let Some(wake) = &self.wake {
            wake();
        }
    }

    fn make_driver(&mut self, id: ConnId) {
        if self.driver == Some(id) {
            return;
        }
        if let Some(old) = self.driver.replace(id) {
            self.send(old, Frame::evt(OP_DEMOTED, json!({})), None);
        }
    }

    fn serve_pending(&mut self, parser: &vt100_ctt::Parser) {
        if self.pending.is_empty() || !parser.is_ground() {
            return;
        }
        let checkpoint = match parser.screen().checkpoint() {
            Ok(bytes) => bytes,
            Err(_) => return,
        };
        let waiting = std::mem::take(&mut self.pending);
        for id in waiting {
            let n = checkpoint.chunks(SNAPSHOT_CHUNK).count().max(1);
            let mut pieces: Vec<&[u8]> = checkpoint.chunks(SNAPSHOT_CHUNK).collect();
            if pieces.is_empty() {
                pieces.push(&[]);
            }
            for (i, piece) in pieces.into_iter().enumerate() {
                let last = i + 1 == n;
                let frame = Frame::evt(OP_SNAPSHOT, blob_payload(json!({"last": last}), piece.len()));
                self.send(id, frame, Some(piece.to_vec()));
            }
            if let Some(conn) = self.conns.get_mut(&id) {
                conn.live = true;
            }
        }
    }
}

/// This process's `(pid, created)`, the identity a hello answers with.
pub fn self_identity() -> crate::Result<(u32, u64)> {
    let status = crate::capsule::self_status(crate::lane::wire::Survival::Normal)?;
    Ok((status.pid, status.created))
}
