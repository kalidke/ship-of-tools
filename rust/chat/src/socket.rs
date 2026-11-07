//! The Socket Mode listener (docs/history/slack-client-plan.md, rules C11 to C13): one websocket opened through
//! `apps.connections.open`, every envelope acknowledged before any event work, normalized events handed on through a
//! bounded queue, one socket at a time, and a visible gap whenever anything was lost. No configuration, no lock, no
//! delivery and no daemon link: the caller hands in the app `Secret` and the bot identity.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::api::{Identity, READ_DEADLINE};
use crate::http::{Http, HttpError, Request, Secret, Websocket};
use crate::normalize::{normalize, valid_id, Context, Normalized};

pub use crate::http::MAX_MESSAGE;
/// Events waiting for the caller; the next one is dropped and latches a gap.
pub const MAX_PENDING: usize = 256;
/// The frames of the waiting events together (an event costs its envelope's size), whatever the count.
pub const MAX_PENDING_BYTES: usize = 8 << 20;
/// An envelope id longer than this is not echoed: Slack's are short, and the acknowledgement stays small.
const MAX_ENVELOPE_ID: usize = 128;
/// A websocket URL from Slack is a host, a path and a ticket.
const MAX_URL: usize = 2048;
pub(crate) const BASE: &str = "https://slack.com/api/";
const FORM: &str = "application/x-www-form-urlencoded";

/// The waits of one listener. Each is a bound, so no wait is open-ended.
#[derive(Clone, Copy)]
pub(crate) struct Timing {
    /// The connection and handshake together.
    pub open: Duration,
    /// A close the peer must answer, then the connection is forced down (observed: a peer that never answers
    /// blocks the read forever otherwise).
    pub close: Duration,
    /// One write (an acknowledgement or a ping): a peer that stops reading can hold the reader for at most this, then
    /// the socket is retired.
    pub write: Duration,
    /// No frame received for this long (a pong or Slack's ping counts; our own ping does not): the connection is lost.
    pub idle: Duration,
    /// Our ping interval.
    pub ping: Duration,
    /// The pause before the first reconnect; it doubles to `backoff_max` while attempts fail and resets after a
    /// `hello`, so a refusing Slack is not hammered and a healthy refresh is back in a second.
    pub backoff_first: Duration,
    pub backoff_max: Duration,
    /// `stop` joins the task within this, then aborts it.
    pub stop: Duration,
}

impl Timing {
    pub(crate) const PRODUCT: Timing = Timing {
        open: Duration::from_secs(15),
        close: Duration::from_secs(2),
        write: Duration::from_secs(5),
        idle: Duration::from_secs(45),
        ping: Duration::from_secs(30),
        backoff_first: Duration::from_secs(1),
        backoff_max: Duration::from_secs(30),
        stop: Duration::from_secs(5),
    };
}

// Our ping and a write that cannot finish must both end well inside the idle bound, or neither could keep a link.
const _: () = assert!(
    Timing::PRODUCT.ping.as_millis() < Timing::PRODUCT.idle.as_millis()
        && Timing::PRODUCT.write.as_millis() < Timing::PRODUCT.idle.as_millis()
);

/// Why a socket could not be opened, or what it last said. Fixed codes: no URL, token or Slack text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketError {
    /// The workspace id is not one the wire can carry.
    BadRequest,
    /// The HTTP core or the websocket failed, a URL that is not a validated Slack host (`destination`) included.
    Http(HttpError),
    /// Slack answered `apps.connections.open` with a status or `ok: false` that refuses the app.
    Rejected,
    /// The answer to `apps.connections.open` has no usable URL.
    Malformed,
}

impl SocketError {
    pub fn code(self) -> &'static str {
        match self {
            SocketError::BadRequest => "bad_request",
            SocketError::Http(e) => e.code(),
            SocketError::Rejected => "rejected",
            SocketError::Malformed => "malformed",
        }
    }
}

impl fmt::Display for SocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for SocketError {}

/// What was lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapCause {
    /// The pending queue was full: an event was dropped (acknowledged, never delivered).
    Overflow,
    /// An event could not be read or represented, or a message passed its bound.
    Failed,
    /// A socket ended, or a new one went live: whatever Slack sent in between is missing.
    Reconnect,
}

/// Every latch of every socket in the process takes the next number, so a gap of one socket never equals a gap of
/// another (a model that outlives its service cannot take the next service's first hello as one it knows).
fn next_seq() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

/// A latched reconciliation requirement. It names the latest loss; `Socket::reconciled` clears it only when nothing
/// was lost since this value was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    seq: u64,
    pub cause: GapCause,
}

impl Gap {
    #[cfg(test)]
    pub(crate) fn for_test(seq: u64, cause: GapCause) -> Gap {
        Gap { seq, cause }
    }
}

/// What `Socket::take` handed over.
#[derive(Debug, Default)]
#[must_use = "the events in a Taken are gone from the socket"]
pub struct Taken {
    pub gap: Option<Gap>,
    pub events: Vec<Normalized>,
}

/// The listener as the caller sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// A socket is open and Slack has said `hello`.
    pub connected: bool,
    /// The last attempt's failure, until a `hello` clears it.
    pub error: Option<SocketError>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<(Normalized, usize)>,
    bytes: usize,
    gap: Option<Gap>,
    status: Status,
}

struct Shared {
    state: Mutex<State>,
    ready: Notify,
}

impl Shared {
    fn latch(&self, cause: GapCause) {
        self.state.lock().unwrap().gap = Some(Gap { seq: next_seq(), cause });
    }

    fn push(&self, event: Normalized, cost: usize) {
        let mut s = self.state.lock().unwrap();
        if s.queue.len() >= MAX_PENDING || s.bytes + cost > MAX_PENDING_BYTES {
            s.gap = Some(Gap { seq: next_seq(), cause: GapCause::Overflow });
            return;
        }
        s.bytes += cost;
        s.queue.push_back((event, cost));
        drop(s);
        self.ready.notify_one();
    }
}

/// One listener for one workspace: it owns its task and its one socket. Dropping it aborts the task; `stop` ends it
/// in order.
pub struct Socket {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    timing: Timing,
}

impl Socket {
    /// Start the listener. `app` is the app-level token; `own` is the bot identity (normalization decides own posts
    /// by it alone). Events for `workspace` arrive through `next`.
    pub fn start(
        http: Http,
        app: Secret,
        own: Identity,
        workspace: &str,
    ) -> Result<Socket, SocketError> {
        Socket::launch(http, app, own, workspace, BASE.into(), Timing::PRODUCT, Box::new(()))
    }

    /// The fixture's logical base URL and short waits; the product has neither.
    #[cfg(test)]
    pub(crate) fn start_for_test(
        http: Http,
        app: Secret,
        own: Identity,
        workspace: &str,
        base: String,
        timing: Timing,
    ) -> Result<Socket, SocketError> {
        Socket::launch(http, app, own, workspace, base, timing, Box::new(()))
    }

    /// The one constructor: `base` and `timing` are the product's from `start`, the fixture's from a test, and `held`
    /// lives exactly as long as the listener's task (dropped when the task ends or is aborted, never while it can run).
    pub(crate) fn launch(
        http: Http,
        app: Secret,
        own: Identity,
        workspace: &str,
        base: String,
        timing: Timing,
        held: Box<dyn Send + Sync>,
    ) -> Result<Socket, SocketError> {
        if !valid_id(workspace) {
            return Err(SocketError::BadRequest);
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            ready: Notify::new(),
        });
        let (stop, stopped) = watch::channel(false);
        let worker = Worker {
            shared: shared.clone(),
            http,
            app,
            own,
            workspace: workspace.into(),
            base,
            timing,
            _held: held,
        };
        let task = tokio::spawn(worker.run(stopped));
        Ok(Socket {
            shared,
            stop,
            task: Some(task),
            timing,
        })
    }

    /// The next pending event, waiting at most `within`. Tests only: in the product `take` is the one way events leave
    /// the socket, so a second reader cannot apply an older event after a newer one.
    #[cfg(test)]
    pub(crate) async fn next(&self, within: Duration) -> Option<Normalized> {
        let end = Instant::now() + within;
        loop {
            {
                let mut s = self.shared.state.lock().unwrap();
                if let Some((event, cost)) = s.queue.pop_front() {
                    s.bytes -= cost;
                    return Some(event);
                }
            }
            tokio::time::timeout_at(end, self.shared.ready.notified())
                .await
                .ok()?;
        }
    }

    /// Every pending event and the gap in force, read under one lock: an event that arrives or is lost after this
    /// call is newer than the gap returned, and a loss after it is a gap this value does not name. The one way events
    /// leave the socket; the caller applies what it takes in the call that took it.
    pub fn take(&self) -> Taken {
        let mut s = self.shared.state.lock().unwrap();
        s.bytes = 0;
        Taken { gap: s.gap, events: s.queue.drain(..).map(|(e, _)| e).collect() }
    }

    /// The reconciliation requirement, when anything was lost since it was last cleared.
    pub fn gap(&self) -> Option<Gap> {
        self.shared.state.lock().unwrap().gap
    }

    /// A complete reconciliation that began after `gap` was read has finished: clear it, unless something was lost
    /// since (then the newer gap stays). Returns whether it cleared.
    pub fn reconciled(&self, gap: Gap) -> bool {
        let mut s = self.shared.state.lock().unwrap();
        if s.gap.is_some_and(|now| now.seq == gap.seq) {
            s.gap = None;
            return true;
        }
        false
    }

    pub fn status(&self) -> Status {
        self.shared.state.lock().unwrap().status
    }

    /// Stop: no further connect, the open socket retired within its close bound, the task joined within `stop`.
    pub async fn stop(self) {
        let within = self.timing.stop;
        self.stop_within(within).await;
    }

    /// `stop` with the join bound given: after it the task is gone, ended or aborted and awaited.
    pub async fn stop_within(mut self, within: Duration) {
        let _ = self.stop.send(true);
        // The handle stays in `self`, so a stop future dropped mid-wait drops the socket and `Drop` aborts the task.
        if let Some(task) = self.task.as_mut() {
            if tokio::time::timeout(within, &mut *task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }
    }

    /// Pending events and their bytes.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> (usize, usize) {
        let s = self.shared.state.lock().unwrap();
        (s.queue.len(), s.bytes)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct Worker {
    shared: Arc<Shared>,
    http: Http,
    app: Secret,
    own: Identity,
    workspace: String,
    base: String,
    timing: Timing,
    /// Dropped with the task's future, after its last poll.
    _held: Box<dyn Send + Sync>,
}

/// What one frame asks of the read loop.
enum Next {
    Continue,
    /// `hello`: the connection is live.
    Hello,
    /// Slack asked for a reconnect (`disconnect`), or the acknowledgement could not be written.
    Retire,
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            break;
        }
    }
}

impl Worker {
    /// Open, serve until the socket ends, retire it, pause, repeat; at most one socket exists at any time because
    /// the next `open` begins only after `serve` has retired the last.
    async fn run(self, mut stop: watch::Receiver<bool>) {
        let mut backoff = self.timing.backoff_first;
        loop {
            if *stop.borrow() {
                break;
            }
            let attempt = tokio::select! {
                biased;
                _ = stopped(&mut stop) => break,
                attempt = self.open() => attempt,
            };
            let wait = match attempt {
                Ok(ws) => {
                    let helloed = self.serve(ws, &mut stop).await;
                    self.set_connected(false);
                    if *stop.borrow() {
                        break;
                    }
                    self.shared.latch(GapCause::Reconnect);
                    if helloed {
                        backoff = self.timing.backoff_first;
                        backoff
                    } else {
                        self.next_backoff(&mut backoff)
                    }
                }
                Err(error) => {
                    self.shared.state.lock().unwrap().status.error = Some(error);
                    self.next_backoff(&mut backoff)
                }
            };
            tokio::select! {
                biased;
                _ = stopped(&mut stop) => break,
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    fn next_backoff(&self, backoff: &mut Duration) -> Duration {
        let wait = *backoff;
        *backoff = (*backoff * 2).min(self.timing.backoff_max);
        wait
    }

    fn set_connected(&self, connected: bool) {
        let mut s = self.shared.state.lock().unwrap();
        s.status.connected = connected;
        if connected {
            s.status.error = None;
        }
    }

    /// `apps.connections.open`, then the websocket at the URL it names, through the one destination policy.
    async fn open(&self) -> Result<Websocket, SocketError> {
        let request = Request::post(
            format!("{}apps.connections.open", self.base),
            READ_DEADLINE,
            FORM,
            Vec::new(),
        )
        .bearer(self.app.clone());
        let response = self.http.send(request).await.map_err(SocketError::Http)?;
        if response.status() != 200 {
            return Err(SocketError::Rejected);
        }
        let v: Value =
            serde_json::from_slice(response.body()).map_err(|_| SocketError::Malformed)?;
        match v.get("ok").and_then(Value::as_bool) {
            Some(true) => {}
            Some(false) => return Err(SocketError::Rejected),
            None => return Err(SocketError::Malformed),
        }
        let url = v
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| u.len() <= MAX_URL)
            .ok_or(SocketError::Malformed)?;
        self.http
            .websocket(url, self.timing.open)
            .await
            .map_err(SocketError::Http)
    }

    /// Read until the socket ends, is asked to, or stop is set; then retire it. Whether Slack said `hello`.
    async fn serve(&self, mut ws: Websocket, stop: &mut watch::Receiver<bool>) -> bool {
        let mut helloed = false;
        let mut ping = tokio::time::interval(self.timing.ping);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping.tick().await;
        // The idle clock restarts on a received frame only: our own ping must not re-arm it.
        let mut last = Instant::now();
        loop {
            let frame = tokio::select! {
                biased;
                _ = stopped(stop) => break,
                _ = tokio::time::sleep_until(last + self.timing.idle) => break,
                _ = ping.tick() => {
                    if !self.write(&mut ws, Message::Ping(Default::default())).await {
                        break;
                    }
                    continue;
                }
                frame = ws.next() => {
                    last = Instant::now();
                    frame
                }
            };
            match frame {
                Some(Ok(Message::Text(text))) => match self.handle(&mut ws, &text).await {
                    Next::Continue => {}
                    Next::Hello => {
                        helloed = true;
                        self.set_connected(true);
                        // Whatever Slack sent before this socket was live is missing: a reconciliation that began
                        // before now cannot clear this.
                        self.shared.latch(GapCause::Reconnect);
                    }
                    Next::Retire => break,
                },
                Some(Ok(Message::Binary(_))) => self.shared.latch(GapCause::Failed),
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(WsError::Capacity(_))) => {
                    self.shared.latch(GapCause::Failed);
                    break;
                }
                Some(Err(_)) => break,
                Some(Ok(_)) => {}
            }
        }
        self.retire(ws).await;
        helloed
    }

    /// One text frame. The acknowledgement goes out before anything else is done with it.
    async fn handle(&self, ws: &mut Websocket, text: &str) -> Next {
        let Ok(frame) = serde_json::from_str::<Value>(text) else {
            self.shared.latch(GapCause::Failed);
            return Next::Continue;
        };
        if let Some(id) = frame.get("envelope_id").and_then(Value::as_str) {
            if id.len() > MAX_ENVELOPE_ID {
                self.shared.latch(GapCause::Failed);
                return Next::Continue;
            }
            let ack = json!({ "envelope_id": id }).to_string();
            if !self.write(ws, Message::Text(ack.into())).await {
                return Next::Retire;
            }
        }
        match frame.get("type").and_then(Value::as_str) {
            Some("hello") => Next::Hello,
            Some("disconnect") => Next::Retire,
            Some("events_api") => {
                self.event(&frame, text.len());
                Next::Continue
            }
            _ => Next::Continue,
        }
    }

    /// A message event becomes the wire record; anything unreadable is a gap, never a silent drop.
    fn event(&self, frame: &Value, cost: usize) {
        let Some(event) = frame.pointer("/payload/event").filter(|e| e.is_object()) else {
            self.shared.latch(GapCause::Failed);
            return;
        };
        if event.get("type").and_then(Value::as_str) != Some("message") {
            return;
        }
        let channel = event.get("channel").and_then(Value::as_str).unwrap_or("");
        let ctx = Context {
            workspace: &self.workspace,
            own_bot_id: self.own.bot_id.as_deref(),
            own_user_id: Some(&self.own.user_id),
            limited: false,
        };
        match normalize(&ctx, channel, event) {
            Normalized::Unsupported => {}
            Normalized::Unrepresentable(_) => self.shared.latch(GapCause::Failed),
            normalized => self.shared.push(normalized, cost),
        }
    }

    async fn write(&self, ws: &mut Websocket, message: Message) -> bool {
        matches!(
            tokio::time::timeout(self.timing.write, ws.send(message)).await,
            Ok(Ok(()))
        )
    }

    /// Close and wait for the peer's answer, within `close`; a peer that does not answer is forced down by dropping
    /// the connection. Returns only when the socket is gone.
    async fn retire(&self, mut ws: Websocket) {
        let _ = tokio::time::timeout(self.timing.close, async {
            let _ = ws.close(None).await;
            while let Some(Ok(_)) = ws.next().await {}
        })
        .await;
        drop(ws);
    }
}
