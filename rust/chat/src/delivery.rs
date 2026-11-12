//! What the delivery decides, pure: from the records handed over, the filings' and passes' returns and `now`, all passed
//! in, which filing to make next and when, when a pass is owed, and how far a channel's boundary moves when a pass has
//! settled. No clock, no file, no thread, no channel: the daemon's owner asks `poll`, runs what it answers and gives each
//! result back. One queue holds every filing, live or from a pass, one at a time, keyed by channel and `ts`, so a post
//! seen twice is filed once.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use sot_protocol::chat::{ChatRecord, Ts};

use crate::mail::{is_mail, Filed};
use crate::normalize::ts_key;
use crate::state::{State, MAX_FILED};
use crate::traverse::{Order, Outcome, Passed};
use crate::watch::Arrived;

/// Filings waiting, all channels together; a live record that finds it full owes a pass instead.
pub const MAX_QUEUED: usize = 512;
/// From one filing's return to the next filing's start, so a registry writer meets free polls after every hold.
pub const FILE_GAP: Duration = Duration::from_millis(200);

/// The delivery's timings as data, for a test to shorten.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// The least time between a pass's result and the next pass's first read.
    pub read_pace: Duration,
    /// A channel is passed this long after its last completed pass, whatever happened.
    pub sweep: Duration,
    /// The wait after a failed pass, doubled by each next failure up to `backoff_max`.
    pub backoff_first: Duration,
    pub backoff_max: Duration,
}

impl Timing {
    pub const PRODUCT: Timing =
        Timing { read_pace: Duration::from_millis(1500), sweep: Duration::from_secs(6 * 3600), backoff_first: Duration::from_secs(30), backoff_max: Duration::from_secs(600) };
}

/// One message to file: the caller resolves the recipient and files `mail::body(&record)`.
#[derive(Debug)]
pub struct Filing {
    pub channel: String,
    pub record: ChatRecord,
    /// True when the socket handed it over, false when a pass found it (the log says which).
    pub live: bool,
}

/// How a filing returned: appended, or not appended for any reason (no row, an ambiguous one, a moved handle, another hub).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Filed,
    Refused,
}

/// What to do next. A `Pass` is made only after the caller resolved the recipient and read the clock for the pass's
/// upper bound; an absent recipient is `pass_skipped`, and costs no read.
#[derive(Debug)]
pub enum Action {
    File(Filing),
    Pass { order: Order, pace: Duration },
    /// Write `boundary` durably to the channel's file; nothing comes back (a stale file only costs reads).
    Commit { channel: String, boundary: Ts },
    /// Nothing now; ask again at the instant if there is one, or when something is handed in or returns.
    Idle(Option<Instant>),
}

struct Chan {
    state: State,
    floor: Ts,
    /// The greatest `ts` handed over for it, any origin: the pass's `Order.seen`.
    seen: Option<Ts>,
    /// A pass is owed for a reason a gap number does not show: a refusal, a drop, an omitted record, `Truncated`, a failure.
    owed: bool,
    /// The gap number the last completed pass began at.
    began: u64,
    /// The end of the last completed pass, for the sweep.
    done: Instant,
    /// The channel's own backoff after failed passes: not passed again before `retry_at`, which the next failure doubles.
    retry_at: Instant,
    backoff: Duration,
}

/// A pass's candidates, once it returned, wait for their filings.
struct Settle {
    mail: Vec<Ts>,
    limit: Ts,
}

struct Pass {
    chan: usize,
    gap: u64,
    settle: Option<Settle>,
}

pub struct Delivery {
    timing: Timing,
    chans: Vec<Chan>,
    queue: VecDeque<(usize, ChatRecord, bool)>,
    inflight: Option<(usize, Ts)>,
    pass: Option<Pass>,
    started: bool,
    gap: u64,
    free_at: Instant,
    /// No pass starts before this: `read_pace` after the last pass's result, whichever channel's.
    pace_at: Instant,
}

impl Delivery {
    /// One channel's state and floor each. Nothing is filed until `start`.
    pub fn new(timing: Timing, now: Instant, channels: impl IntoIterator<Item = (State, Ts)>) -> Delivery {
        let chans = channels.into_iter().map(|(state, floor)| Chan { state, floor, seen: None, owed: false, began: 0, done: now, retry_at: now, backoff: timing.backoff_first }).collect();
        Delivery { timing, chans, queue: VecDeque::new(), inflight: None, pass: None, started: false, gap: 0, free_at: now, pace_at: now }
    }

    /// The start's read of the inboxes is in: its filings join the sets, and filing may begin. True when a set went past
    /// `MAX_FILED`.
    pub fn start<'a>(&mut self, filings: impl IntoIterator<Item = &'a Filed> + Clone) -> bool {
        self.started = true;
        self.chans.iter_mut().fold(false, |over, c| c.state.merge(filings.clone()) | over)
    }

    /// Each new record of the socket is queued when it is mail above its channel's boundary. An omitted one, or one that
    /// finds the queue or the channel's set full, is not filed and owes a pass instead. The gap number only rises.
    pub fn hand_over(&mut self, arrived: Arrived) {
        self.gap = self.gap.max(arrived.gap);
        for rec in arrived.records {
            let Some(i) = self.chans.iter().position(|c| c.state.channel() == rec.channel) else { continue };
            let seen = &mut self.chans[i].seen;
            if seen.as_ref().map_or(true, |s| ts_key(&rec.ts) > ts_key(s)) {
                *seen = Some(rec.ts.clone());
            }
            if !is_mail(&rec, &self.chans[i].floor) {
                continue;
            }
            // An omitted record is never queued, and owes a pass only if it is not settled already.
            let owes = if rec.omitted { !self.chans[i].state.contains(&rec.ts) } else { !self.enqueue(i, rec, true) };
            self.chans[i].owed |= owes;
        }
    }

    /// Whether `rec` is now filed, queued or in flight (true), or was not queued for want of room (false). Only a live
    /// record is held back by a full set: a pass's candidates are queued whatever it holds, and their commit prunes it.
    fn enqueue(&mut self, i: usize, rec: ChatRecord, live: bool) -> bool {
        let state = &self.chans[i].state;
        if state.contains(&rec.ts) || self.pending(i, &rec.ts) {
            return true;
        }
        if self.queue.len() >= MAX_QUEUED || (live && state.count() >= MAX_FILED) {
            return false;
        }
        self.queue.push_back((i, rec, live));
        true
    }

    /// Whether a filing of channel `i` whose `ts` satisfies `f` is queued or in flight.
    fn queued(&self, i: usize, f: impl Fn(&Ts) -> bool) -> bool {
        self.queue.iter().any(|(c, r, _)| *c == i && f(&r.ts)) || self.inflight.as_ref().is_some_and(|(c, t)| *c == i && f(t))
    }

    fn pending(&self, i: usize, ts: &Ts) -> bool {
        self.queued(i, |t| ts_key(t) == ts_key(ts))
    }

    /// Whether a pass is owed to `channel` for a reason of its own (a gap number or the sweep owes one besides).
    #[cfg(test)]
    pub fn owed(&self, channel: &str) -> bool {
        self.chans.iter().any(|c| c.owed && c.state.channel() == channel)
    }

    /// The next thing to do. First a commit, once a returned pass has settled. Then the oldest queued filing, once the
    /// start's read is in, none is in flight and `FILE_GAP` has passed since the last returned (a queued record that was
    /// filed since it entered is dropped here, unissued). Then a pass, if none is running and one is due: owed by a gap
    /// number above the one the channel's last completed pass began at, by a refusal, a drop, an omitted record or a failure,
    /// or by the sweep, and not before the backoff or `read_pace` after the last pass.
    pub fn poll(&mut self, now: Instant) -> Action {
        if let Some(commit) = self.settle(now) {
            return commit;
        }
        if !self.started {
            return Action::Idle(None);
        }
        let mut wake = None;
        if self.inflight.is_none() && !self.queue.is_empty() {
            if now < self.free_at {
                wake = Some(self.free_at);
            }
            while now >= self.free_at {
                let Some((i, record, live)) = self.queue.pop_front() else { break };
                if !self.chans[i].state.contains(&record.ts) {
                    self.inflight = Some((i, record.ts.clone()));
                    return Action::File(Filing { channel: self.chans[i].state.channel().into(), record, live });
                }
            }
        }
        if self.pass.is_none() {
            let due = |c: &Chan| (if c.owed || self.gap > c.began { c.retry_at } else { (c.done + self.timing.sweep).max(c.retry_at) }).max(self.pace_at);
            if let Some((i, at, _)) = self.chans.iter().enumerate().map(|(i, c)| (i, due(c), c.done)).min_by_key(|(_, at, done)| (*at, *done)) {
                if at > now {
                    return Action::Idle(Some(wake.map_or(at, |w: Instant| w.min(at))));
                }
                let c = &mut self.chans[i];
                c.owed = false;
                self.pass = Some(Pass { chan: i, gap: self.gap, settle: None });
                let order = Order { state: c.state.clone(), floor: c.floor.clone(), seen: c.seen.clone() };
                return Action::Pass { order, pace: self.timing.read_pace };
            }
        }
        Action::Idle(wake)
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

    /// The pass's result. Whatever it was, its mail joins the queue (a candidate already filed, queued or in flight is not
    /// queued twice); only a `Complete` or `Truncated` one counts as completed, and settles into a commit once every
    /// filing it waits for has returned. Any other outcome commits nothing and owes the channel another pass on the
    /// backoff, no sooner than a `RateLimited` one's `Retry-After`.
    pub fn passed(&mut self, now: Instant, passed: Passed) {
        let Some(Pass { chan: i, gap, settle: None }) = self.pass else { return };
        let truncated = passed.outcome == Outcome::Truncated;
        self.pace_at = now + self.timing.read_pace;
        let mail: Vec<Ts> = passed.mail.iter().map(|r| r.ts.clone()).collect();
        for rec in passed.mail {
            self.enqueue(i, rec, false);
        }
        if !(truncated || passed.outcome == Outcome::Complete) {
            self.pass = None;
            let wait = if let Outcome::RateLimited { retry_after } = passed.outcome { retry_after } else { Duration::ZERO };
            return self.fail(now, i, wait);
        }
        (self.chans[i].began, self.chans[i].done) = (gap, now);
        self.chans[i].owed |= truncated;
        // A `Truncated` pass handed over only its oldest candidates, so it commits no further than the last of them.
        let last = mail.last().filter(|t| truncated && ts_key(t) < ts_key(&passed.horizon));
        let limit = last.unwrap_or(&passed.horizon).clone();
        self.pass = Some(Pass { chan: i, gap, settle: Some(Settle { mail, limit }) });
    }

    /// The pass was not made, the recipient being absent: nothing was read, and it is owed again on the backoff.
    pub fn pass_skipped(&mut self, now: Instant) {
        if let Some(Pass { chan, settle: None, .. }) = self.pass.take() {
            self.fail(now, chan, Duration::ZERO);
        }
    }

    fn fail(&mut self, now: Instant, i: usize, wait: Duration) {
        let (c, max) = (&mut self.chans[i], self.timing.backoff_max);
        c.owed = true;
        c.retry_at = now + c.backoff.max(wait);
        c.backoff = (c.backoff * 2).min(max);
    }

    /// Once every filing of a returned pass's candidates, and of every record of the channel at or below the limit, has
    /// returned (so the boundary never reaches a post still pending, which the recheck at issue would drop), the commit:
    /// to the limit when every candidate at or below it was filed, else to the last one filed before the first that was
    /// not, and not at all when the oldest was not. The limit is the horizon, or for a `Truncated` pass no further than
    /// its last candidate.
    fn settle(&mut self, now: Instant) -> Option<Action> {
        let Some(Pass { chan: i, settle: Some(s), .. }) = &self.pass else { return None };
        let i = *i;
        if self.queued(i, |t| ts_key(t) <= ts_key(&s.limit)) {
            return None;
        }
        let Some(Pass { settle: Some(s), .. }) = self.pass.take() else { return None };
        let c = &mut self.chans[i];
        let (mut to, mut clean) = (None, true);
        for ts in s.mail.iter().filter(|t| ts_key(t) <= ts_key(&s.limit)) {
            if !c.state.contains(ts) {
                clean = false;
                break;
            }
            to = Some(ts);
        }
        let to = if clean { Some(&s.limit) } else { to };
        let moved = to.filter(|t| ts_key(t) > ts_key(c.state.boundary())).cloned();
        if let Some(t) = &moved {
            c.state.commit(t);
        }
        if clean {
            self.chans[i].backoff = self.timing.backoff_first;
        } else {
            self.fail(now, i, Duration::ZERO);
        }
        moved.map(|boundary| Action::Commit { channel: self.chans[i].state.channel().into(), boundary })
    }
}
