//! The recovery traversal of one channel: the channel read back from the pass's upper bound to the channel's floor in
//! fresh reads, and what to hand over (the mail not yet filed) and how far the boundary may move (the horizon). It
//! files nothing and commits nothing: the caller's delivery decides both from `Passed`. The thread walk is not here.
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
/// A runaway guard: about ten million top-level messages at 999 a read.
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
    /// The latest the boundary may be committed to, fixed from the first page: the greatest `ts` or `latest_reply` seen
    /// less `MARGIN`, never past U and never below the boundary.
    pub horizon: Ts,
    /// Every mail record read above the boundary and not in the filed set, oldest first, at most `MAX_PASS_MAIL`.
    pub mail: Vec<ChatRecord>,
    pub outcome: Outcome,
    pub reads: u32,
    /// Some page carried Slack's `is_limited`; information only, it ends nothing.
    pub limited: bool,
}

/// Walks the channel back from `upper` to the floor, one read at least `pace` after the one before.
pub async fn pass(api: &Api, order: &Order, upper: &Ts, pace: Duration) -> Passed {
    walk(api, order, upper, pace, MAX_PASS_READS).await
}

pub(crate) async fn walk(api: &Api, order: &Order, upper: &Ts, pace: Duration, max_reads: u32) -> Passed {
    let (state, floor) = (&order.state, &order.floor);
    let mut got = Passed { horizon: state.boundary().clone(), mail: Vec::new(), outcome: Outcome::Complete, reads: 0, limited: false };
    if ts_key(floor) >= ts_key(upper) {
        return got;
    }
    let (mut kept, mut cut) = (BTreeMap::new(), false);
    let (mut latest, mut limit, mut first, mut last) = (upper.clone(), MAX_SCAN, true, None);
    got.outcome = loop {
        if got.reads >= max_reads {
            break Outcome::TooLong;
        }
        let Ok(query) = Bounds::new(Some(floor.as_str()), Some(latest.as_str()), None).and_then(|b| ScanQuery::new(state.channel(), None, b, limit)) else {
            break Outcome::ReadFailed;
        };
        if let Some(at) = last {
            sleep_until(at + pace).await;
        }
        (last, got.reads) = (Some(Instant::now()), got.reads + 1);
        let page = match api.scan(&query).await {
            Ok(page) => page,
            // A page too large to arrive in time or in size is asked for again at half the size.
            Err(ApiError::Http(HttpError::TooLarge | HttpError::Timeout)) if limit > 1 => {
                limit /= 2;
                continue;
            }
            Err(ApiError::Limited { retry_after, .. }) => break Outcome::RateLimited { retry_after },
            Err(ApiError::Closed) => break Outcome::Stopped,
            Err(_) => break Outcome::ReadFailed,
        };
        if first {
            (first, got.horizon) = (false, horizon(order, upper, &page));
        }
        got.limited |= page.limited;
        for rec in page.records {
            if is_mail(&rec, floor) && !state.contains(&rec.ts) {
                kept.entry(ts_key(&rec.ts)).or_insert(rec);
                if kept.len() > MAX_PASS_MAIL {
                    kept.pop_last();
                    cut = true;
                }
            }
        }
        if !page.has_more {
            break if cut { Outcome::Truncated } else { Outcome::Complete };
        }
        // The walk moves back to the page's oldest raw `ts`; a page that does not lower it would read the same page forever.
        match page.oldest {
            Some(oldest) if ts_key(&oldest) < ts_key(&latest) => latest = oldest,
            _ => break Outcome::ReadFailed,
        }
    };
    got.mail = kept.into_values().collect();
    got
}

/// The greatest of the delivery's `seen` and the first page's `ts` and `latest_reply` values, less `MARGIN`, capped at
/// `upper` (a `latest_reply` may lie above the read's own `latest`) and never below the boundary.
fn horizon(order: &Order, upper: &Ts, page: &ScanPage) -> Ts {
    let micros = |t: &Ts| ts_key(t).0 * 1_000_000 + u128::from(ts_key(t).1);
    let threads = page.threads.iter().flat_map(|(ts, latest)| [ts, latest]);
    let top = order.seen.iter().chain(&page.newest).chain(threads).map(micros).max();
    let at = top.map_or(0, |t| t.saturating_sub(MARGIN.as_micros())).min(micros(upper)).max(micros(order.state.boundary()));
    Ts::parse(&format!("{}.{:06}", at / 1_000_000, at % 1_000_000)).unwrap_or_else(|_| order.state.boundary().clone())
}
