//! The run side of the row socket: what the supervisor's main loop feeds and drains.

use super::proto::{AttachRes, OP_ATTACH, OP_DEMOTED, OP_OUTPUT, OP_SNAPSHOT, OUTPUT_CHUNK, SNAPSHOT_CHUNK};
use super::MAX_CONNECTIONS;
use serde_json::json;
use sot_protocol::Frame;
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// What a connection's writer sends: a frame with its blob.
pub(super) enum Out {
    Frame(Frame, Option<Vec<u8>>),
}

/// What queuing a frame did to the connection.
pub(super) enum Push {
    Queued,
    /// The output queued for it was dropped: it has to be shown the screen again.
    Resync,
    /// Even its answers cannot drain: it is cut.
    Cut,
}

/// What a queued frame is to its queue.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Output,
    /// A piece of a screen, `first` on its first: a screen the writer has not begun is superseded by a resync's.
    Screen { first: bool },
    /// A request's reply or a demotion.
    Answer,
}

/// One connection's queue of frames, in order. Output is droppable: when too much of it waits, all of it is dropped,
/// with every screen the writer has not begun, and the viewer is resynced with a fresh snapshot. Answers (a request's
/// reply, a demotion) are never dropped; a viewer that cannot even drain those is cut. Memory per connection is bounded
/// either way.
pub(super) struct OutQueue {
    state: Mutex<OutState>,
    ready: std::sync::Condvar,
}

#[derive(Default)]
struct OutState {
    items: VecDeque<(Out, Class)>,
    droppable: usize,
    /// No more frames are taken; the writer drains what is queued, then ends.
    finished: bool,
    /// The writer ends now.
    aborted: bool,
}

impl OutQueue {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(OutState::default()), ready: std::sync::Condvar::new() })
    }

    fn lock(&self) -> MutexGuard<'_, OutState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn push(&self, frame: Frame, blob: Option<Vec<u8>>, class: Class) -> Push {
        let mut state = self.lock();
        if state.finished || state.aborted {
            return Push::Queued;
        }
        if class == Class::Output {
            if state.droppable >= QUEUE_FRAMES {
                // Only the queue's front can be a screen the writer has begun; its rest is still sent.
                let mut begun = true;
                state.items.retain(|(_, c)| {
                    begun &= *c == Class::Screen { first: false };
                    begun || *c == Class::Answer
                });
                state.droppable = 0;
                return Push::Resync;
            }
            state.droppable += 1;
        } else if state.items.len() - state.droppable >= ANSWER_FRAMES {
            return Push::Cut;
        }
        state.items.push_back((Out::Frame(frame, blob), class));
        self.ready.notify_one();
        Push::Queued
    }

    /// The writer's next frame, waiting for one; `None` once the queue is finished and drained, or aborted.
    pub(super) fn pop(&self) -> Option<Out> {
        let mut state = self.lock();
        loop {
            if state.aborted {
                return None;
            }
            if let Some((out, class)) = state.items.pop_front() {
                if class == Class::Output {
                    state.droppable -= 1;
                }
                return Some(out);
            }
            if state.finished {
                return None;
            }
            state = self.ready.wait(state).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// The connection is closed: what is queued is still sent.
    fn finish(&self) {
        self.lock().finished = true;
        self.ready.notify_all();
    }

    /// The writer ends now, what is queued unsent.
    fn abort(&self) {
        let mut state = self.lock();
        state.aborted = true;
        state.items.clear();
        state.droppable = 0;
        self.ready.notify_all();
    }
}

/// A request a connection hands the main loop; the loop does it and answers.
pub enum RowRequest {
    Input { bytes: Vec<u8>, run: String, reply: mpsc::SyncSender<Result<usize, String>> },
    Resize { cols: u16, rows: u16, reply: mpsc::SyncSender<Result<(), String>> },
}

/// How long a connection waits for the main loop to take its input.
const INPUT_BOUND: Duration = Duration::from_secs(2);
/// How long a connection has to say hello, in all.
const HELLO_BOUND: Duration = Duration::from_secs(5);
/// The frames a connection may have queued; a viewer that does not drain them is dropped.
pub(super) const QUEUE_FRAMES: usize = 256;
/// The answers (replies, demotions, snapshots) a connection may have queued at most; a viewer past it is cut.
pub(super) const ANSWER_FRAMES: usize = 64;
/// Requests the main loop has not yet taken, at most; one more is refused as busy.
pub(super) const REQUEST_QUEUE: usize = 64;

pub(super) type ConnId = u64;

struct Conn {
    queue: Arc<OutQueue>,
    /// The snapshot was sent: output flows to it.
    live: bool,
    /// Ends the connection's stream, so a reader or writer blocked on it returns.
    closer: Arc<dyn Fn() + Send + Sync>,
}

/// A connection's slot. It is released when both of the connection's threads have ended, never earlier, so at most
/// `MAX_CONNECTIONS` connections exist, whatever has been evicted.
pub(super) struct Slot(Arc<RowRun>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.lock().slots -= 1;
    }
}

/// Refusal texts; the server names each by its code.
pub(super) const BUSY: &str = "busy: the row's request queue is full";
pub(super) const UNKNOWN: &str = "delivery unknown: the terminal did not answer in time";
pub(super) const RUN_ENDED: &str = "run ended: the input was sent to an earlier run";

pub(super) fn code_of(msg: &str) -> &'static str {
    if msg.starts_with("busy") {
        "busy"
    } else if msg.starts_with("delivery unknown") {
        "unknown"
    } else if msg.starts_with("run ended") {
        "run_ended"
    } else {
        "refused"
    }
}

struct Inner {
    run: String,
    phase: String,
    phase_source: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    conns: HashMap<ConnId, Conn>,
    /// Slots held: connections whose threads have not both ended.
    slots: usize,
    /// A wake is pending: the loop has not drained since the last one was sent.
    wake_pending: bool,
    hello_bound: Duration,
    input_bound: Duration,
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
                phase_source: None,
                conns: HashMap::new(),
                slots: 0,
                wake_pending: false,
                hello_bound: HELLO_BOUND,
                input_bound: INPUT_BOUND,
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
        let source = self.lock().phase_source.clone();
        match source {
            Some(f) => f(),
            None => self.lock().phase.clone(),
        }
    }

    /// Where a hello reads the phase from, so the row reports the one answer the supervisor's status gives.
    pub fn set_phase_source(&self, source: Box<dyn Fn() -> String + Send + Sync>) {
        self.lock().phase_source = Some(Arc::from(source));
    }

    pub fn set_phase(&self, phase: &str) {
        self.lock().phase = phase.to_string();
    }

    /// A new agent start is a new run: the id changes, and connections of the old run keep their pen and their output.
    /// Input still queued for the run that ended is answered `run ended` now, never written into the next.
    pub fn set_run(&self, run: impl Into<String>) {
        let mut inner = self.lock();
        inner.run = run.into();
        inner.answer_stale_input();
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

    /// At most `max` requests, for one pass of the loop. What is left arms the wake again.
    pub fn take_requests(&self, max: usize) -> Vec<RowRequest> {
        let mut inner = self.lock();
        inner.answer_stale_input();
        let n = inner.requests.len().min(max);
        let taken: Vec<RowRequest> = inner.requests.drain(..n).collect();
        inner.wake_pending = false;
        if !inner.requests.is_empty() {
            inner.arm_wake();
        }
        taken
    }

    /// Connections open now, counted until both of a connection's threads have ended.
    pub fn open_connections(&self) -> usize {
        self.lock().slots
    }

    /// Shortens the hello's and the input's bounds for a test.
    #[doc(hidden)]
    pub fn set_bounds_for_test(&self, hello: Duration, input: Duration) {
        let mut inner = self.lock();
        inner.hello_bound = hello;
        inner.input_bound = input;
    }

    #[cfg(unix)]
    pub(super) fn hello_bound(&self) -> Duration {
        self.lock().hello_bound
    }

    /// Ends every connection's stream: the run is over for them. Their threads end on their own; the server joins them.
    pub fn close_all(&self) {
        let mut inner = self.lock();
        for conn in inner.conns.values() {
            conn.queue.abort();
            (conn.closer)();
        }
        inner.conns.clear();
        inner.pending.clear();
        inner.driver = None;
    }

    /// A slot for a new connection, `None` while eight are held. The slot goes back when the guard drops.
    pub(super) fn open(self: &Arc<Self>, closer: Arc<dyn Fn() + Send + Sync>) -> Option<(ConnId, Arc<OutQueue>, Slot)> {
        let mut inner = self.lock();
        if inner.slots >= MAX_CONNECTIONS {
            return None;
        }
        inner.slots += 1;
        let id = inner.next_conn;
        inner.next_conn += 1;
        let queue = OutQueue::new();
        inner.conns.insert(id, Conn { queue: Arc::clone(&queue), live: false, closer });
        Some((id, queue, Slot(Arc::clone(self))))
    }

    pub(super) fn close(&self, id: ConnId) {
        let mut inner = self.lock();
        if let Some(conn) = inner.conns.remove(&id) {
            conn.queue.finish();
        }
        inner.pending.retain(|p| *p != id);
        if inner.driver == Some(id) {
            inner.driver = None;
        }
    }

    pub(super) fn send(&self, id: ConnId, frame: Frame, blob: Option<Vec<u8>>) {
        self.lock().send(id, frame, blob);
    }

    /// Admits an attach: a driver's own size is applied to the terminal first, and only then is the reply sent and the
    /// screen queued, so the snapshot is cut at the driver's geometry. A reattach as a watcher gives up the pen.
    pub(super) fn attach(&self, id: ConnId, req_id: u64, cols: u16, rows: u16, driver: bool) -> Result<(), String> {
        if driver {
            check_size(cols, rows)?;
            self.resize_terminal(cols, rows)?;
        }
        let mut inner = self.lock();
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
        } else if inner.driver == Some(id) {
            inner.driver = None;
        }
        Ok(())
    }

    pub(super) fn take(&self, id: ConnId) {
        self.lock().make_driver(id);
    }

    /// Asks the loop to resize the terminal and waits for its answer: a failure is a failure.
    fn resize_terminal(&self, cols: u16, rows: u16) -> Result<(), String> {
        let (reply, answered) = mpsc::sync_channel(1);
        let bound = {
            let mut inner = self.lock();
            inner.request(RowRequest::Resize { cols, rows, reply })?;
            inner.input_bound
        };
        match answered.recv_timeout(bound) {
            Ok(result) => result,
            Err(_) => Err("the terminal did not take the resize in time".into()),
        }
    }

    /// Hands bytes to the main loop and waits for it to write them. A full queue is `busy`; no answer in time is
    /// `delivery unknown`, since the loop may still write them.
    pub(super) fn input(&self, id: ConnId, bytes: Vec<u8>) -> Result<usize, String> {
        let (reply, answered) = mpsc::sync_channel(1);
        let bound = {
            let mut inner = self.lock();
            if inner.driver != Some(id) {
                return Err("this connection is not the driver".into());
            }
            let run = inner.run.clone();
            inner.request(RowRequest::Input { bytes, run, reply })?;
            inner.input_bound
        };
        match answered.recv_timeout(bound) {
            Ok(result) => result,
            Err(_) => Err(UNKNOWN.into()),
        }
    }

    pub(super) fn resize(&self, id: ConnId, cols: u16, rows: u16) -> Result<(), String> {
        check_size(cols, rows)?;
        if self.lock().driver != Some(id) {
            return Err("this connection is not the driver".into());
        }
        self.resize_terminal(cols, rows)
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
    /// Queues a frame for a connection. Output that piles up is dropped and the viewer resynced with a fresh snapshot at
    /// the next ground state; a connection that cannot even drain its answers is cut, never waited for: its stream is
    /// shut down, so its blocked threads return and end.
    fn send(&mut self, id: ConnId, frame: Frame, blob: Option<Vec<u8>>) {
        let class = if frame.op == OP_OUTPUT { Class::Output } else { Class::Answer };
        self.send_as(id, frame, blob, class);
    }

    fn send_as(&mut self, id: ConnId, frame: Frame, blob: Option<Vec<u8>>, class: Class) {
        let Some(conn) = self.conns.get(&id) else { return };
        if class == Class::Output && !conn.live {
            // Not live: it is waiting for its screen, which already holds this output.
            return;
        }
        match conn.queue.push(frame, blob, class) {
            Push::Queued => {}
            Push::Resync => {
                if let Some(conn) = self.conns.get_mut(&id) {
                    conn.live = false;
                }
                if !self.pending.contains(&id) {
                    self.pending.push(id);
                }
            }
            Push::Cut => {
                conn.queue.abort();
                (conn.closer)();
                self.conns.remove(&id);
                self.pending.retain(|p| *p != id);
                if self.driver == Some(id) {
                    self.driver = None;
                }
            }
        }
    }

    /// Queues a request for the loop, refused as busy when the queue is full.
    fn request(&mut self, req: RowRequest) -> Result<(), String> {
        if self.requests.len() >= REQUEST_QUEUE {
            return Err(BUSY.into());
        }
        self.requests.push_back(req);
        self.arm_wake();
        Ok(())
    }

    /// One wake is pending at most; the loop's drain clears it.
    fn arm_wake(&mut self) {
        if self.wake_pending {
            return;
        }
        self.wake_pending = true;
        if let Some(wake) = &self.wake {
            wake();
        }
    }

    /// Input queued for an earlier run is answered `run ended`, and never reaches the loop.
    fn answer_stale_input(&mut self) {
        let run = self.run.clone();
        let (stale, keep): (Vec<_>, Vec<_>) =
            self.requests.drain(..).partition(|r| matches!(r, RowRequest::Input { run: sent, .. } if *sent != run));
        self.requests = keep.into();
        for request in stale {
            if let RowRequest::Input { reply, .. } = request {
                let _ = reply.send(Err(RUN_ENDED.into()));
            }
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
                self.send_as(id, frame, Some(piece.to_vec()), Class::Screen { first: i == 0 });
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

impl std::fmt::Debug for RowRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowRun").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
