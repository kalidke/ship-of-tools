//! The model of the watched channels (docs/history/slack-client-plan.md, rule M5): what each watched channel holds, kept
//! true across the socket's events, a snapshot and the socket's gaps. A pure model with no task and no call: its caller
//! reads the socket and the API and tells it what happened. Registration (`watch`) precedes the snapshot (`begin`);
//! while a snapshot is pending, live changes go into a bounded overlay applied after it.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use sot_protocol::chat::{ChatRecord, Ts};

use crate::normalize::{valid_id, Normalized};
use crate::socket::Gap;

/// Channels watched at once.
pub const MAX_CHANNELS: usize = 32;
/// Messages kept per channel; the oldest leave first.
pub const MAX_KEPT: usize = 100;
/// Live changes kept per channel while its snapshot is pending; one more forces a resync.
pub const MAX_OVERLAY: usize = 128;

/// A validated timestamp's value: seconds, then microseconds. `Ts` is `<1 to 20 digits>.<six digits>`, so the digits
/// fit and the comparison is numeric (`9.000000` precedes `10.000000`, `007.000000` equals `7.000000`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key(u128, u32);

fn key(ts: &Ts) -> Key {
    let (secs, frac) = ts.as_str().split_once('.').unwrap_or_default();
    Key(secs.parse().unwrap_or(0), frac.parse().unwrap_or(0))
}

/// The one total order on timestamps: numeric, so two spellings of one value are equal.
pub fn ts_cmp(a: &Ts, b: &Ts) -> Ordering {
    key(a).cmp(&key(b))
}

/// Why a channel could not be watched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchError {
    /// `MAX_CHANNELS` are already watched.
    Full,
    /// Not a channel id.
    Invalid,
}

/// Where a watched channel stands. Only `Complete` says its messages are the channel's latest page and every change
/// since; `Needed` and `Pending` show what is held, which may be stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// A snapshot is owed: not yet begun, or lost.
    Needed,
    /// A snapshot was begun and not answered.
    Pending,
    Complete,
}

/// What `loaded` did with a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loaded {
    Complete,
    /// A gap or an overlay overflow made the snapshot unsafe: nothing was kept, and a new one is owed.
    Resync,
    /// Not the channel's pending snapshot (unwatched, or another one began since): ignored.
    Stale,
}

enum Change {
    Put { record: ChatRecord, edited: bool },
    Delete(Key),
}

enum Phase {
    Needed,
    /// `read`: the gap the model knew when the snapshot began.
    Pending { ticket: u64, read: Option<Gap>, overlay: Vec<Change>, overflow: bool },
    Complete,
}

struct Channel {
    phase: Phase,
    messages: BTreeMap<Key, ChatRecord>,
    /// Messages at or below this are older than what is held: an edit of one the model lacks is not a message to add.
    floor: Option<Key>,
}

impl Channel {
    fn apply(&mut self, change: Change) {
        match change {
            Change::Delete(k) => {
                self.messages.remove(&k);
            }
            Change::Put { record, edited } => {
                let k = key(&record.ts);
                if edited {
                    if !self.messages.contains_key(&k) && self.floor.is_some_and(|f| k <= f) {
                        return;
                    }
                    self.messages.insert(k, record);
                } else {
                    // A new message never replaces what is held: the snapshot may already carry a later edit.
                    self.messages.entry(k).or_insert(record);
                }
                self.trim();
            }
        }
    }

    fn trim(&mut self) {
        while self.messages.len() > MAX_KEPT {
            if let Some((k, _)) = self.messages.pop_first() {
                self.floor = self.floor.max(Some(k));
            }
        }
    }
}

/// The watched channels of one workspace and the socket gap they answer to.
#[derive(Default)]
pub struct Watches {
    gap: Option<Gap>,
    tickets: u64,
    channels: HashMap<String, Channel>,
}

impl Watches {
    pub fn new() -> Watches {
        Watches::default()
    }

    /// Register a channel, before its snapshot. Watching one already watched changes nothing.
    pub fn watch(&mut self, channel: &str) -> Result<(), WatchError> {
        if !valid_id(channel) {
            return Err(WatchError::Invalid);
        }
        if !self.channels.contains_key(channel) {
            if self.channels.len() >= MAX_CHANNELS {
                return Err(WatchError::Full);
            }
            let ch = Channel { phase: Phase::Needed, messages: BTreeMap::new(), floor: None };
            self.channels.insert(channel.into(), ch);
        }
        Ok(())
    }

    /// Forget a channel, whatever it was doing; an answer to its pending snapshot is `Stale`.
    pub fn unwatch(&mut self, channel: &str) {
        self.channels.remove(channel);
    }

    pub fn state(&self, channel: &str) -> Option<State> {
        self.channels.get(channel).map(|c| match c.phase {
            Phase::Needed => State::Needed,
            Phase::Pending { .. } => State::Pending,
            Phase::Complete => State::Complete,
        })
    }

    /// The held messages, oldest first.
    pub fn messages(&self, channel: &str) -> Vec<&ChatRecord> {
        self.channels.get(channel).map(|c| c.messages.values().collect()).unwrap_or_default()
    }

    /// The socket's gap as the caller read it. A gap this model has not seen makes every complete channel owe a
    /// snapshot, and one pending is refused when it is answered. Call it before trusting any state.
    pub fn sync(&mut self, current: Option<Gap>) {
        if current.is_some() && current != self.gap {
            self.gap = current;
            for c in self.channels.values_mut() {
                if matches!(c.phase, Phase::Complete) {
                    c.phase = Phase::Needed;
                }
            }
        }
    }

    /// Begin a channel's snapshot, after `sync` and after the channel was registered. The ticket goes back with the
    /// answer; `None` when the channel is unwatched or its snapshot is already pending or done.
    pub fn begin(&mut self, channel: &str) -> Option<u64> {
        let read = self.gap;
        let c = self.channels.get_mut(channel).filter(|c| matches!(c.phase, Phase::Needed))?;
        self.tickets += 1;
        c.phase = Phase::Pending { ticket: self.tickets, read, overlay: Vec::new(), overflow: false };
        Some(self.tickets)
    }

    /// The snapshot is the channel's latest page, `more` saying older messages exist. The overlay is applied after it,
    /// so a live edit or delete wins over the snapshot's older content.
    pub fn loaded(&mut self, channel: &str, ticket: u64, records: Vec<ChatRecord>, more: bool) -> Loaded {
        let gap = self.gap;
        let Some(c) = self.channels.get_mut(channel) else { return Loaded::Stale };
        let Phase::Pending { ticket: t, read, overlay, overflow } = &mut c.phase else { return Loaded::Stale };
        if *t != ticket {
            return Loaded::Stale;
        }
        if *overflow || *read != gap {
            c.phase = Phase::Needed;
            return Loaded::Resync;
        }
        let overlay = std::mem::take(overlay);
        c.messages = records.into_iter().map(|r| (key(&r.ts), r)).collect();
        c.floor = None;
        c.trim();
        if more {
            c.floor = c.floor.max(c.messages.keys().next().copied());
        }
        for change in overlay {
            c.apply(change);
        }
        c.phase = Phase::Complete;
        Loaded::Complete
    }

    /// The snapshot could not be had (an error, a stopped service, a dropped call): a new one is owed.
    pub fn failed(&mut self, channel: &str, ticket: u64) {
        if let Some(c) = self.channels.get_mut(channel) {
            if matches!(c.phase, Phase::Pending { ticket: t, .. } if t == ticket) {
                c.phase = Phase::Needed;
            }
        }
    }

    /// One socket event. True when a watch took it; an event for a channel nobody watches, a thread reply (replies
    /// belong to the thread, and a parent's count is not tracked) and a non-message are not kept.
    pub fn apply(&mut self, event: &Normalized) -> bool {
        let (channel, change) = match event {
            Normalized::New(r) | Normalized::Edited(r) => {
                if key(&r.thread_ts) != key(&r.ts) {
                    return false;
                }
                let edited = matches!(event, Normalized::Edited(_));
                (&r.channel, Change::Put { record: r.clone(), edited })
            }
            Normalized::Deleted { channel, ts, .. } => (channel, Change::Delete(key(ts))),
            _ => return false,
        };
        let Some(c) = self.channels.get_mut(channel) else { return false };
        match &mut c.phase {
            Phase::Pending { overlay, overflow, .. } => {
                if overlay.len() >= MAX_OVERLAY {
                    *overflow = true;
                    overlay.clear();
                } else if !*overflow {
                    overlay.push(change);
                }
            }
            _ => c.apply(change),
        }
        true
    }

    /// Clear the socket's gap, through `clear` (`Socket::reconciled`), when every watch has resynced from a snapshot
    /// begun after that gap was read. `clear` refuses when something was lost since, and the gap stays.
    pub fn reconcile(&mut self, clear: impl FnOnce(Gap) -> bool) -> bool {
        let Some(gap) = self.gap else { return false };
        if self.channels.values().all(|c| matches!(c.phase, Phase::Complete)) && clear(gap) {
            self.gap = None;
            return true;
        }
        false
    }
}
