//! The recovery traversal of one channel: the channel read back from the pass's upper bound to the channel's floor in
//! fresh reads, each thread with a reply after the boundary read back to the boundary the same way, and what to hand
//! over (the mail not yet filed) and how far the boundary may move (the horizon). It
//! files nothing and commits nothing: the caller's delivery decides both from `Passed`.
//! The walk is the one SlackClaw measured (its facts note, "The walk back with no cursor, measured" and "Posts arriving
//! during reads and the walk (P-ORDER), measured"): every read carries both bounds and no cursor, the first with the
//! upper bound U, each next with `latest` set to the oldest raw `ts` of the page before.

use std::collections::BTreeMap;
use std::time::Duration;

use sot_protocol::chat::{ChatRecord, Ts};
use tokio::time::{sleep_until, Instant};

use crate::api::{Api, ApiError, Bounds, ScanPage, ScanQuery, MAX_SCAN};
use crate::http::HttpError;
use crate::mail::is_mail;
use crate::normalize::ts_key;
use crate::state::State;

/// How far below the newest `ts` a pass has seen its horizon lies. Measured unneeded (a post below a read's `latest` was
/// never missing by that read), kept in reserve for a visibility window the measurement could not resolve.
pub const MARGIN: Duration = Duration::from_secs(2);
/// A runaway guard on the reads of one pass, the channel's pages and its threads' alike: the channel's pages alone
/// reach it at about ten million top-level messages at 999 a read.
pub const MAX_PASS_READS: u32 = 10_000;
/// The mail one pass hands over, the oldest first; past it the pass says `Truncated`.
pub const MAX_PASS_MAIL: usize = 200;

/// What one channel's pass starts from, as a snapshot: the channel's state (boundary, filed set, channel id), its floor,
/// and the greatest `ts` the delivery was handed for it before the pass's first read (an event's).
#[derive(Clone, Debug)]
pub struct Order {
    pub state: State,
    pub floor: Ts,
    pub seen: Option<Ts>,
}

/// How a pass ended. Only `Complete` and `Truncated` let the caller commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every page Slack gives was read.
    Complete,
    /// Complete, but more mail was found than `MAX_PASS_MAIL`: the caller owes another pass at once.
    Truncated,
    /// A read failed, or a page with more behind it could not move the walk.
    ReadFailed,
    /// A 429 or `ratelimited`: the wait is the caller's backoff, and no read waits it out here.
    RateLimited { retry_after: Duration },
    /// `MAX_PASS_READS` reads were made.
    TooLong,
    /// The service closed the client.
    Stopped,
}

/// What a pass found, whatever its outcome.
#[derive(Clone, Debug)]
pub struct Passed {
    /// The latest the boundary may be committed to, fixed from the first page alone: the greatest `ts` or `latest_reply`
    /// seen less `MARGIN`, capped at U, and never below the boundary, which wins when it is above U. A `Complete` pass
    /// may be committed to it. A `Truncated` one handed over only the oldest `MAX_PASS_MAIL` records, so it may be
    /// committed only as far as the mail it handed over was filed; mail past the cap at or below `horizon` is not in
    /// `mail`.
    pub horizon: Ts,
    /// Every mail record read above the boundary and not in the filed set, oldest first, at most `MAX_PASS_MAIL`.
    pub mail: Vec<ChatRecord>,
    pub outcome: Outcome,
    pub reads: u32,
    /// Some page carried Slack's `is_limited`; information only, it ends nothing.
    pub limited: bool,
}

/// Walks the channel back from `upper` to the floor, and the thread of every parent it sees with a reply after the
/// boundary, one read at least `pace` after the one before.
pub async fn pass(api: &Api, order: &Order, upper: &Ts, pace: Duration) -> Passed {
    walk(api, order, upper, pace, MAX_PASS_READS).await
}

/// One walk's place: the thread it reads (none: the channel) and the `latest` of its next read.
struct Back {
    parent: Option<Ts>,
    latest: Ts,
}

impl Back {
    /// Moves on from `page`: whether there is more behind it. A page that says so and does not lower `latest` would read
    /// the same page forever, so the pass fails instead.
    fn onward(&mut self, has_more: bool, oldest: Option<Ts>) -> Result<bool, Outcome> {
        match oldest {
            _ if !has_more => Ok(false),
            Some(oldest) if ts_key(&oldest) < ts_key(&self.latest) => {
                self.latest = oldest;
                Ok(true)
            }
            _ => Err(Outcome::ReadFailed),
        }
    }
}

/// A pass in progress: the reads made, their pace and guard, and the candidates kept so far.
struct Run<'a> {
    api: &'a Api,
    order: &'a Order,
    pace: Duration,
    max_reads: u32,
    last: Option<Instant>,
    limit: u16,
    kept: BTreeMap<(u128, u32), ChatRecord>,
    cut: bool,
    got: Passed,
}

pub(crate) async fn walk(api: &Api, order: &Order, upper: &Ts, pace: Duration, max_reads: u32) -> Passed {
    let got = Passed { horizon: order.state.boundary().clone(), mail: Vec::new(), outcome: Outcome::Complete, reads: 0, limited: false };
    let mut run = Run { api, order, pace, max_reads, last: None, limit: MAX_SCAN, kept: BTreeMap::new(), cut: false, got };
    if ts_key(&order.floor) < ts_key(upper) {
        run.got.outcome = match run.channel(upper).await {
            Ok(()) if run.cut => Outcome::Truncated,
            Ok(()) => Outcome::Complete,
            Err(outcome) => outcome,
        };
    }
    run.got.mail = run.kept.into_values().collect();
    run.got
}

impl Run<'_> {
    /// The channel back to the floor; after each page, the threads of its parents with a reply after the boundary.
    async fn channel(&mut self, upper: &Ts) -> Result<(), Outcome> {
        let (order, mut at, mut first) = (self.order, Back { parent: None, latest: upper.clone() }, true);
        let boundary = ts_key(order.state.boundary());
        loop {
            let page = self.read(&mut at, &order.floor).await?;
            if first {
                (first, self.got.horizon) = (false, horizon(order, upper, &page));
            }
            self.keep(page.records);
            let more = at.onward(page.has_more, page.oldest)?;
            // A reply is mail only after the boundary, so a parent whose newest reply is not after it has none to read,
            // and none lies below U when the boundary is not below it.
            for (parent, _) in page.threads.iter().filter(|(_, newest)| boundary < ts_key(upper) && ts_key(newest) > boundary) {
                self.thread(parent, upper).await?;
            }
            if !more {
                return Ok(());
            }
        }
    }

    /// One thread back from `upper` to the boundary, its parent never the next `latest`.
    async fn thread(&mut self, parent: &Ts, upper: &Ts) -> Result<(), Outcome> {
        let (order, mut at) = (self.order, Back { parent: Some(parent.clone()), latest: upper.clone() });
        loop {
            let page = self.read(&mut at, order.state.boundary()).await?;
            self.keep(page.records);
            if !at.onward(page.has_more, page.oldest)? {
                return Ok(());
            }
        }
    }

    /// One read for `at` above `oldest`, at least `pace` after the one before and counted by the guard; a page too large
    /// to arrive in time or in size is asked for again at half the size, for the rest of the pass.
    async fn read(&mut self, at: &mut Back, oldest: &Ts) -> Result<ScanPage, Outcome> {
        loop {
            if self.got.reads >= self.max_reads {
                return Err(Outcome::TooLong);
            }
            let parent = at.parent.as_ref().map(Ts::as_str);
            let bounds = Bounds::new(Some(oldest.as_str()), Some(at.latest.as_str()), None);
            let Ok(query) = bounds.and_then(|b| ScanQuery::new(self.order.state.channel(), parent, b, self.limit)) else {
                return Err(Outcome::ReadFailed);
            };
            if let Some(last) = self.last {
                sleep_until(last + self.pace).await;
            }
            (self.last, self.got.reads) = (Some(Instant::now()), self.got.reads + 1);
            return match self.api.scan(&query).await {
                Ok(page) => {
                    self.got.limited |= page.limited;
                    Ok(page)
                }
                Err(ApiError::Http(HttpError::TooLarge | HttpError::Timeout)) if self.limit > 1 => {
                    self.limit /= 2;
                    continue;
                }
                Err(ApiError::Limited { retry_after, .. }) => Err(Outcome::RateLimited { retry_after }),
                Err(ApiError::Closed) => Err(Outcome::Stopped),
                Err(_) => Err(Outcome::ReadFailed),
            };
        }
    }

    /// Keeps the page's mail not yet filed, one record a `ts` (a broadcast comes back in the channel and in its thread),
    /// the oldest `MAX_PASS_MAIL`.
    fn keep(&mut self, records: Vec<ChatRecord>) {
        for rec in records {
            if is_mail(&rec, &self.order.floor) && !self.order.state.contains(&rec.ts) {
                self.kept.entry(ts_key(&rec.ts)).or_insert(rec);
                if self.kept.len() > MAX_PASS_MAIL {
                    self.kept.pop_last();
                    self.cut = true;
                }
            }
        }
    }
}

/// The greatest of the delivery's `seen` and the first page's `ts` and `latest_reply` values, less `MARGIN`, capped at
/// `upper` (a `latest_reply` may lie above the read's own `latest`) and never below the boundary.
fn horizon(order: &Order, upper: &Ts, page: &ScanPage) -> Ts {
    let micros = |t: &Ts| ts_key(t).0 * 1_000_000 + u128::from(ts_key(t).1);
    let threads = page.threads.iter().flat_map(|(ts, latest)| [ts, latest]);
    let top = order.seen.iter().chain(&page.newest).chain(threads).map(micros).max();
    let at = top.map_or(0, |t| t.saturating_sub(MARGIN.as_micros())).min(micros(upper)).max(micros(order.state.boundary()));
    // `at` is at most the greater of two `Ts` values and shown with six fraction digits, so it parses.
    Ts::parse(&format!("{}.{:06}", at / 1_000_000, at % 1_000_000)).expect("a horizon between two timestamps is one")
}
