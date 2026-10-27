//! The channel table: every channel's queues, credit and ends, and the rules a peer's frame must keep, all under one
//! lock with the link's state, so a link's end and a channel's open or read are one decision.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::task::Waker;

use sot_protocol::ops::link::{LinkCloseReason, CHUNK, MAX_CHANNELS, WINDOW};

/// The most bytes a channel's user may hand the link before the link's writer has taken them.
pub(super) const OUTBOUND_CAP: usize = CHUNK;
/// Bytes a consumer read are handed back as credit once this many have gathered.
const CREDIT_STEP: u32 = CHUNK as u32;

/// Which end of the link this table is: the window opens channels, the router accepts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Window,
    Router,
}

/// How a link ended: `clean` for an end either side meant (a closed window, channel 0's end), else a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct End {
    pub(super) clean: bool,
    pub(super) reason: String,
}

impl End {
    pub(super) fn clean(reason: impl Into<String>) -> Self {
        Self { clean: true, reason: reason.into() }
    }

    pub(super) fn failed(reason: impl Into<String>) -> Self {
        Self { clean: false, reason: reason.into() }
    }
}

/// A link's state, shared by its reader, its writer and every channel's user.
pub(super) struct Shared {
    pub(super) table: Mutex<Table>,
    /// Woken at every change a blocking face may wait on.
    pub(super) changed: Condvar,
    /// The link's one writer waits here for a frame to send.
    pub(super) writer: tokio::sync::Notify,
    /// Woken when the link ends.
    pub(super) ended: tokio::sync::Notify,
}

pub(super) struct Table {
    side: Side,
    /// Why the link ended; `None` while it is up.
    down: Option<End>,
    chans: BTreeMap<u32, Chan>,
    /// The highest channel number opened on this link: a frame for one above it names a channel never opened.
    highest: u32,
    /// The link ops waiting for the writer, sent before any channel's data.
    ops: VecDeque<Op>,
    /// The channel the writer served last, so the next turn starts after it.
    turn: u32,
    /// The next request id of a `link.open` (the window's).
    next_req: u64,
}

/// A link op waiting for the writer.
pub(super) enum Op {
    Open { ch: u32, id: u64 },
    Opened { ch: u32, id: u64 },
    Refused { ch: u32, id: u64, code: &'static str, error: String },
    Credit { ch: u32, bytes: u32 },
}

/// What the writer sends next.
pub(super) enum Out {
    Op(Op),
    Data { ch: u32, bytes: Vec<u8> },
    Close { ch: u32, reason: LinkCloseReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Opening,
    Open,
    Refused { code: String, error: String },
}

#[derive(Default)]
struct Wakers {
    read: Option<Waker>,
    write: Option<Waker>,
    open: Option<Waker>,
}

impl Wakers {
    fn wake(&mut self) {
        for waker in [self.read.take(), self.write.take(), self.open.take()].into_iter().flatten() {
            waker.wake();
        }
    }
}

struct Chan {
    phase: Phase,
    /// Bytes received and not yet read, oldest first; `head` bytes of the first are read already.
    inbound: VecDeque<Vec<u8>>,
    head: usize,
    /// What the peer may still send before this end returns credit.
    allowance: u32,
    /// Bytes read since the last credit this end returned.
    unreturned: u32,
    /// Bytes this end's user wrote that the writer has not sent.
    outbound: VecDeque<u8>,
    /// What this end may still send.
    credit: u32,
    /// The peer's close, once received.
    peer_closed: Option<LinkCloseReason>,
    /// This end's close, once decided: an `eof` is sent after the bytes before it, a `reset` at once.
    local_close: Option<LinkCloseReason>,
    close_sent: bool,
    /// The user's handle is gone, or the opener gave up.
    released: bool,
    cancelled: bool,
    wakers: Wakers,
}

impl Chan {
    fn new(phase: Phase) -> Self {
        let credit = if phase == Phase::Open { WINDOW } else { 0 };
        Self {
            phase,
            inbound: VecDeque::new(),
            head: 0,
            allowance: credit,
            unreturned: 0,
            outbound: VecDeque::new(),
            credit,
            peer_closed: None,
            local_close: None,
            close_sent: false,
            released: false,
            cancelled: false,
            wakers: Wakers::default(),
        }
    }

    fn grant(&mut self) {
        self.phase = Phase::Open;
        self.credit = WINDOW;
        self.allowance = WINDOW;
    }

    /// Whether either end has closed it: data still arriving for it is dropped.
    fn closed(&self) -> bool {
        self.peer_closed.is_some() || self.local_close.is_some()
    }

    /// Whether nothing more will be done with it: its user is gone and its close was sent or received.
    fn finished(&self) -> bool {
        self.released && (self.close_sent || self.peer_closed.is_some() || matches!(self.phase, Phase::Refused { .. }))
    }
}

/// What a read found.
pub(super) enum Read {
    Bytes(usize),
    Eof,
    Failed(Failure),
    Pending,
}

/// What a write found.
pub(super) enum Write {
    Accepted(usize),
    Failed(Failure),
    Pending,
}

/// Why a channel's read or write failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Failure {
    /// The link ended: its reason.
    LinkDown(String),
    /// This end cancelled the channel.
    Cancelled,
    /// The far end reset or refused the channel, or this end closed it.
    Closed(String),
}

impl Failure {
    pub(super) fn into_io(self) -> std::io::Error {
        match self {
            Failure::LinkDown(why) => std::io::Error::new(std::io::ErrorKind::NotConnected, format!("the host's link is down: {why}")),
            // Not `Interrupted`, which copy loops retry.
            Failure::Cancelled => std::io::Error::other("the channel was cancelled"),
            Failure::Closed(why) => std::io::Error::new(std::io::ErrorKind::ConnectionReset, why),
        }
    }
}

/// How an open ended.
pub(super) enum Opened {
    Open,
    Refused { code: String, error: String },
    LinkDown,
    Pending,
}

impl Table {
    fn new(side: Side) -> Self {
        let mut chans = BTreeMap::new();
        // Channel 0 is the control channel, open from the link's hello on both ends.
        chans.insert(0, Chan::new(Phase::Open));
        Self { side, down: None, chans, highest: 0, ops: VecDeque::new(), turn: 0, next_req: 1 }
    }

    pub(super) fn is_down(&self) -> bool {
        self.down.is_some()
    }

    pub(super) fn down(&self) -> Option<&End> {
        self.down.as_ref()
    }

    /// The first end wins; every channel's waiter wakes to find it.
    fn end(&mut self, end: End) -> bool {
        if self.down.is_some() {
            return false;
        }
        self.down = Some(end);
        for chan in self.chans.values_mut() {
            chan.wakers.wake();
        }
        true
    }

    fn down_reason(&self) -> String {
        self.down.as_ref().map_or_else(String::new, |e| e.reason.clone())
    }

    /// Drops a channel nothing more will be done with.
    fn reap(&mut self, ch: u32) {
        if self.chans.get(&ch).is_some_and(Chan::finished) {
            self.chans.remove(&ch);
        }
    }

    // --- The peer's frames. A violation is the `Err`, and it ends the link. ---

    /// A channel number above the highest opened was never opened: the peer broke the link's rules.
    fn known(&self, ch: u32, what: &str) -> Result<(), String> {
        if ch > self.highest {
            return Err(format!("a {what} for channel {ch}, which was never opened"));
        }
        Ok(())
    }

    pub(super) fn on_data(&mut self, ch: u32, bytes: Vec<u8>) -> Result<(), String> {
        self.known(ch, "link.data")?;
        let Some(chan) = self.chans.get_mut(&ch) else { return Ok(()) };
        if chan.closed() || chan.cancelled {
            return Ok(());
        }
        let n = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        if n > chan.allowance {
            return Err(format!("{n} bytes on channel {ch}, beyond its credit of {}", chan.allowance));
        }
        chan.allowance -= n;
        if !bytes.is_empty() {
            chan.inbound.push_back(bytes);
        }
        if let Some(waker) = chan.wakers.read.take() {
            waker.wake();
        }
        Ok(())
    }

    pub(super) fn on_credit(&mut self, ch: u32, bytes: u32) -> Result<(), String> {
        self.known(ch, "link.credit")?;
        let Some(chan) = self.chans.get_mut(&ch) else { return Ok(()) };
        if chan.closed() {
            return Ok(());
        }
        if u64::from(chan.credit) + u64::from(bytes) > u64::from(WINDOW) {
            return Err(format!("{bytes} bytes of credit on channel {ch}, beyond its window"));
        }
        chan.credit += bytes;
        Ok(())
    }

    /// The peer closed `ch`; `true` when it was channel 0, whose end ends the link.
    pub(super) fn on_close(&mut self, ch: u32, reason: LinkCloseReason) -> Result<bool, String> {
        self.known(ch, "link.close")?;
        let Some(chan) = self.chans.get_mut(&ch) else { return Ok(false) };
        if chan.peer_closed.is_none() {
            chan.peer_closed = Some(reason);
            // The peer reads no more: what this end still held for it goes, and no close of this end's follows.
            chan.outbound.clear();
            chan.close_sent = true;
            chan.wakers.wake();
        }
        self.reap(ch);
        Ok(ch == 0)
    }

    /// The router's answer to the window's open of `ch`.
    pub(super) fn on_open_answer(&mut self, ch: u32, answer: Result<(), (String, String)>) -> Result<(), String> {
        if self.side != Side::Window {
            return Err("a link.open answer sent to the router".to_string());
        }
        let Some(chan) = self.chans.get_mut(&ch).filter(|c| c.phase == Phase::Opening) else {
            return Err(format!("an answer to an open of channel {ch}, which was not asked"));
        };
        match answer {
            Ok(()) => {
                chan.grant();
                if chan.released {
                    // The opener gave up while the answer was on its way: the channel goes at once.
                    chan.local_close = Some(LinkCloseReason::Reset);
                }
            }
            Err((code, error)) => chan.phase = Phase::Refused { code, error },
        }
        chan.wakers.wake();
        self.reap(ch);
        Ok(())
    }

    /// The window's open of `ch` (request `id`), on the router: `true` when the router should connect it.
    pub(super) fn on_open_request(&mut self, ch: u32, id: u64) -> Result<bool, String> {
        if self.side != Side::Router {
            return Err("a link.open sent to the window".to_string());
        }
        if ch <= self.highest {
            return Err(format!("channel {ch} opened again or out of order (highest {})", self.highest));
        }
        self.highest = ch;
        if self.chans.len() >= MAX_CHANNELS {
            let error = format!("this link holds {MAX_CHANNELS} channels already");
            self.ops.push_back(Op::Refused { ch, id, code: "too_many_channels", error });
            return Ok(false);
        }
        self.chans.insert(ch, Chan::new(Phase::Opening));
        Ok(true)
    }

    /// The router's connect for `ch` finished: `true` when the channel is open and its pump should start.
    pub(super) fn connected(&mut self, ch: u32, id: u64, result: Result<(), String>) -> bool {
        if self.is_down() {
            return false;
        }
        let Some(chan) = self.chans.get_mut(&ch) else { return false };
        match result {
            Ok(()) if !chan.closed() => {
                chan.grant();
                self.ops.push_back(Op::Opened { ch, id });
                true
            }
            Ok(()) => {
                self.chans.remove(&ch);
                false
            }
            Err(error) => {
                self.chans.remove(&ch);
                self.ops.push_back(Op::Refused { ch, id, code: "unreachable", error });
                false
            }
        }
    }

    // --- The writer. ---

    /// The next frame to send: the link ops first, then each channel with something to say, one chunk each in turn.
    pub(super) fn next_out(&mut self) -> Option<Out> {
        if let Some(op) = self.ops.pop_front() {
            return Some(Out::Op(op));
        }
        let order: Vec<u32> = self.chans.range(self.turn + 1..).chain(self.chans.range(..=self.turn)).map(|(ch, _)| *ch).collect();
        for ch in order {
            let chan = self.chans.get_mut(&ch).expect("a listed channel");
            if let Some(reason) = chan.local_close.filter(|_| !chan.close_sent) {
                if chan.outbound.is_empty() || reason == LinkCloseReason::Reset || chan.phase != Phase::Open {
                    chan.close_sent = true;
                    chan.outbound.clear();
                    chan.wakers.wake();
                    self.turn = ch;
                    self.reap(ch);
                    return Some(Out::Close { ch, reason });
                }
            }
            if chan.phase == Phase::Open && !chan.close_sent && chan.credit > 0 && !chan.outbound.is_empty() {
                let n = chan.outbound.len().min(chan.credit as usize).min(CHUNK);
                let bytes: Vec<u8> = chan.outbound.drain(..n).collect();
                chan.credit -= n as u32;
                if let Some(waker) = chan.wakers.write.take() {
                    waker.wake();
                }
                self.turn = ch;
                return Some(Out::Data { ch, bytes });
            }
        }
        None
    }

    // --- A channel's user. ---

    pub(super) fn read(&mut self, ch: u32, buf: &mut [u8], waker: Option<&Waker>) -> Read {
        let down = self.down_reason();
        let is_down = self.is_down();
        let Some(chan) = self.chans.get_mut(&ch) else { return Read::Eof };
        if chan.cancelled {
            return Read::Failed(Failure::Cancelled);
        }
        if let Some(front) = chan.inbound.front() {
            let n = (front.len() - chan.head).min(buf.len());
            buf[..n].copy_from_slice(&front[chan.head..chan.head + n]);
            chan.head += n;
            if chan.head == front.len() {
                chan.inbound.pop_front();
                chan.head = 0;
            }
            chan.unreturned += n as u32;
            if chan.unreturned >= CREDIT_STEP && !chan.closed() && !is_down {
                let bytes = std::mem::take(&mut chan.unreturned);
                chan.allowance += bytes;
                self.ops.push_back(Op::Credit { ch, bytes });
            }
            return Read::Bytes(n);
        }
        match chan.peer_closed {
            Some(LinkCloseReason::Eof) => return Read::Eof,
            Some(reason) => return Read::Failed(Failure::Closed(format!("the far end ended channel {ch} ({reason:?})"))),
            None => {}
        }
        if chan.local_close.is_some() {
            return Read::Eof;
        }
        if is_down {
            return Read::Failed(Failure::LinkDown(down));
        }
        if let Phase::Refused { code, error } = &chan.phase {
            return Read::Failed(Failure::Closed(format!("{code}: {error}")));
        }
        if let Some(waker) = waker {
            chan.wakers.read = Some(waker.clone());
        }
        Read::Pending
    }

    pub(super) fn write(&mut self, ch: u32, data: &[u8], waker: Option<&Waker>) -> Write {
        let down = self.down_reason();
        let is_down = self.is_down();
        let Some(chan) = self.chans.get_mut(&ch) else { return Write::Failed(Failure::Closed(format!("channel {ch} is gone"))) };
        if chan.cancelled {
            return Write::Failed(Failure::Cancelled);
        }
        if is_down {
            return Write::Failed(Failure::LinkDown(down));
        }
        if chan.peer_closed.is_some() {
            return Write::Failed(Failure::Closed(format!("the far end closed channel {ch}")));
        }
        if chan.local_close.is_some() {
            return Write::Failed(Failure::Closed(format!("channel {ch} is closed")));
        }
        let room = OUTBOUND_CAP.saturating_sub(chan.outbound.len());
        if room == 0 || data.is_empty() {
            if data.is_empty() {
                return Write::Accepted(0);
            }
            if let Some(waker) = waker {
                chan.wakers.write = Some(waker.clone());
            }
            return Write::Pending;
        }
        let n = room.min(data.len());
        chan.outbound.extend(&data[..n]);
        Write::Accepted(n)
    }

    /// Whether every byte written was handed to the writer, or the channel can send no more.
    pub(super) fn flushed(&mut self, ch: u32, waker: Option<&Waker>) -> bool {
        let is_down = self.is_down();
        let Some(chan) = self.chans.get_mut(&ch) else { return true };
        if chan.outbound.is_empty() || is_down || chan.cancelled || chan.peer_closed.is_some() || chan.close_sent {
            return true;
        }
        if let Some(waker) = waker {
            chan.wakers.write = Some(waker.clone());
        }
        false
    }

    /// This end is done writing: a close follows the bytes before it.
    pub(super) fn shutdown(&mut self, ch: u32) {
        if let Some(chan) = self.chans.get_mut(&ch) {
            if !chan.closed() {
                chan.local_close = Some(LinkCloseReason::Eof);
            }
        }
    }

    pub(super) fn cancel(&mut self, ch: u32) {
        if let Some(chan) = self.chans.get_mut(&ch) {
            chan.cancelled = true;
            if !chan.closed() && chan.phase == Phase::Open {
                chan.local_close = Some(LinkCloseReason::Reset);
            }
            chan.outbound.clear();
            chan.wakers.wake();
        }
    }

    /// The user's last handle is gone: a channel not yet closed closes, `eof` when nothing it wrote is waiting, else
    /// `reset` with what waited discarded.
    pub(super) fn release(&mut self, ch: u32) {
        if let Some(chan) = self.chans.get_mut(&ch) {
            chan.released = true;
            if !chan.closed() && chan.phase == Phase::Open {
                let reason = if chan.outbound.is_empty() { LinkCloseReason::Eof } else { LinkCloseReason::Reset };
                chan.local_close = Some(reason);
            }
        }
        self.reap(ch);
    }

    /// The window's open: a new channel number, sent at once. `Err` (the link's end) while the link is down.
    pub(super) fn begin_open(&mut self) -> Result<u32, String> {
        if let Some(end) = &self.down {
            return Err(end.reason.clone());
        }
        let ch = self.highest + 1;
        self.highest = ch;
        let id = self.next_req;
        self.next_req += 1;
        self.chans.insert(ch, Chan::new(Phase::Opening));
        self.ops.push_back(Op::Open { ch, id });
        Ok(ch)
    }

    pub(super) fn poll_open(&mut self, ch: u32, waker: Option<&Waker>) -> Opened {
        let is_down = self.is_down();
        let Some(chan) = self.chans.get_mut(&ch) else { return Opened::LinkDown };
        match &chan.phase {
            Phase::Open => Opened::Open,
            Phase::Refused { code, error } => {
                let refused = Opened::Refused { code: code.clone(), error: error.clone() };
                chan.released = true;
                self.reap(ch);
                refused
            }
            Phase::Opening if is_down => Opened::LinkDown,
            Phase::Opening => {
                if let Some(waker) = waker {
                    chan.wakers.open = Some(waker.clone());
                }
                Opened::Pending
            }
        }
    }

    /// The opener of `ch` gave up before its answer: the answer, when it comes, closes the channel.
    pub(super) fn abandon(&mut self, ch: u32) {
        if let Some(chan) = self.chans.get_mut(&ch) {
            if chan.phase == Phase::Opening {
                chan.released = true;
                return;
            }
        }
        self.release(ch);
    }
}

impl Shared {
    pub(super) fn new(side: Side) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            table: Mutex::new(Table::new(side)),
            changed: Condvar::new(),
            writer: tokio::sync::Notify::new(),
            ended: tokio::sync::Notify::new(),
        })
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Tells every waiter a change happened: the blocking faces, and the writer.
    pub(super) fn changed(&self) {
        self.changed.notify_all();
        self.writer.notify_one();
    }

    /// Runs `f` under the lock and tells every waiter.
    pub(super) fn with<T>(&self, f: impl FnOnce(&mut Table) -> T) -> T {
        let out = f(&mut self.lock());
        self.changed();
        out
    }

    /// Ends the link, once: every channel's reader gets the bytes already queued and then the end, every writer the
    /// end, and an open the end. A failure is logged once.
    pub(super) fn end(&self, end: End) {
        let first = {
            let mut table = self.lock();
            let reason = end.reason.clone();
            let clean = end.clean;
            table.end(end).then_some((reason, clean))
        };
        if let Some((reason, clean)) = first {
            if clean {
                tracing::info!(target: "sot::link", %reason, "link ended");
            } else {
                tracing::warn!(target: "sot::link", %reason, "link ended");
            }
            self.changed();
            self.ended.notify_waiters();
        }
    }

    pub(super) fn is_down(&self) -> bool {
        self.lock().is_down()
    }

    /// Returns once the link has ended.
    pub(super) async fn wait_ended(&self) {
        loop {
            let notified = self.ended.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_down() {
                return;
            }
            notified.await;
        }
    }
}
