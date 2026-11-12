//! What the delivery decides, pure: from the records handed over, the filings' returns and `now`, all passed in, which
//! filing to make next and when. No clock, no file, no thread, no channel: the daemon's owner asks `poll`, runs what it
//! answers and gives each result back. One queue holds every filing, one at a time, keyed by channel and `ts`, so a post
//! seen twice is filed once. This is the filing half; the pass half (a pass's result, the schedule of passes, the commit
//! of the boundary) is unbuilt.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use sot_protocol::chat::{ChatRecord, Ts};

use crate::mail::{is_mail, Filed};
use crate::normalize::ts_key;
use crate::state::{State, MAX_FILED};
use crate::watch::Arrived;

/// Filings waiting, all channels together; a live record that finds it full owes a pass instead.
pub const MAX_QUEUED: usize = 512;
/// From one filing's return to the next filing's start, so a registry writer meets free polls after every hold.
pub const FILE_GAP: Duration = Duration::from_millis(200);

/// One message to file: the caller resolves the recipient and files `mail::body(&record)`.
#[derive(Debug)]
pub struct Filing {
    pub channel: String,
    pub record: ChatRecord,
}

/// How a filing returned: appended, or not appended for any reason (no row, an ambiguous one, a moved handle, another hub).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Filed,
    Refused,
}

/// What to do next.
#[derive(Debug)]
pub enum Action {
    File(Filing),
    /// Nothing now; ask again at the instant if there is one, or when something is handed in or returns.
    Idle(Option<Instant>),
}

struct Chan {
    state: State,
    floor: Ts,
    /// A pass is owed: a filing was refused, a record was dropped or came omitted.
    owed: bool,
}

pub struct Delivery {
    chans: Vec<Chan>,
    queue: VecDeque<(usize, ChatRecord)>,
    inflight: Option<(usize, Ts)>,
    started: bool,
    free_at: Instant,
}

impl Delivery {
    /// One channel's state and floor each. Nothing is filed until `start`.
    pub fn new(now: Instant, channels: impl IntoIterator<Item = (State, Ts)>) -> Delivery {
        let chans = channels.into_iter().map(|(state, floor)| Chan { state, floor, owed: false }).collect();
        Delivery { chans, queue: VecDeque::new(), inflight: None, started: false, free_at: now }
    }

    /// The start's read of the inboxes is in: its filings join the sets, and filing may begin. True when a set went past
    /// `MAX_FILED`.
    pub fn start<'a>(&mut self, filings: impl IntoIterator<Item = &'a Filed> + Clone) -> bool {
        self.started = true;
        self.chans.iter_mut().fold(false, |over, c| c.state.merge(filings.clone()) | over)
    }

    /// Each new record of the socket is queued when it is mail above its channel's boundary. An omitted one, or one that
    /// finds the queue or the channel's set full, is not filed and owes a pass instead.
    pub fn hand_over(&mut self, arrived: Arrived) {
        for rec in arrived.records {
            let Some(i) = self.chans.iter().position(|c| c.state.channel() == rec.channel) else { continue };
            if !is_mail(&rec, &self.chans[i].floor) {
                continue;
            }
            // An omitted record is never queued, and owes a pass only if it is not settled already.
            let owes = if rec.omitted { !self.chans[i].state.contains(&rec.ts) } else { !self.enqueue(i, rec) };
            self.chans[i].owed |= owes;
        }
    }

    /// Whether `rec` is now filed, queued or in flight (true), or was not queued for want of room (false).
    fn enqueue(&mut self, i: usize, rec: ChatRecord) -> bool {
        let state = &self.chans[i].state;
        if state.contains(&rec.ts) || self.pending(i, &rec.ts) {
            return true;
        }
        if self.queue.len() >= MAX_QUEUED || state.count() >= MAX_FILED {
            return false;
        }
        self.queue.push_back((i, rec));
        true
    }

    fn pending(&self, i: usize, ts: &Ts) -> bool {
        let same = |c: usize, t: &Ts| c == i && ts_key(t) == ts_key(ts);
        self.queue.iter().any(|(c, r)| same(*c, &r.ts)) || self.inflight.as_ref().is_some_and(|(c, t)| same(*c, t))
    }

    /// Whether a pass is owed to `channel` for a reason of its own (the pass half adds the gap and the sweep).
    pub fn owed(&self, channel: &str) -> bool {
        self.chans.iter().any(|c| c.owed && c.state.channel() == channel)
    }

    /// The next thing to do: the oldest queued filing, once the start's read is in, none is in flight and `FILE_GAP` has
    /// passed since the last returned. A queued record that was filed since it entered is dropped here, unissued.
    pub fn poll(&mut self, now: Instant) -> Action {
        if !self.started || self.inflight.is_some() || self.queue.is_empty() {
            return Action::Idle(None);
        }
        if now < self.free_at {
            return Action::Idle(Some(self.free_at));
        }
        while let Some((i, record)) = self.queue.pop_front() {
            if !self.chans[i].state.contains(&record.ts) {
                self.inflight = Some((i, record.ts.clone()));
                return Action::File(Filing { channel: self.chans[i].state.channel().into(), record });
            }
        }
        Action::Idle(None)
    }

    /// The filing in flight returned: its `ts` joins the filed set only if it was appended (true when that took the set
    /// past `MAX_FILED`); a refusal owes the channel a pass. The next filing waits `FILE_GAP` from `now`.
    pub fn filed(&mut self, now: Instant, verdict: Verdict) -> bool {
        self.free_at = now + FILE_GAP;
        let Some((i, ts)) = self.inflight.take() else { return false };
        match verdict {
            Verdict::Filed => self.chans[i].state.insert(&ts),
            Verdict::Refused => {
                self.chans[i].owed = true;
                false
            }
        }
    }
}
