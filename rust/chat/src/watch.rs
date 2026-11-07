//! The model of the watched channels (docs/history/slack-client-plan.md, rule M5): what each watched channel holds, kept
//! true across the socket's events, a snapshot and the socket's gaps. A pure model with no task and no call: its caller
//! reads the socket and the API and tells it what happened. Registration (`watch`) precedes the snapshot (`begin`);
//! while a snapshot is pending, live changes go into a bounded overlay applied after it.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sot_protocol::chat::ChatRecord;

use crate::normalize::{ts_key, valid_id, Normalized};
use crate::service::Service;
use crate::socket::{Gap, Taken};

/// Channels watched at once.
pub const MAX_CHANNELS: usize = 32;
/// Messages kept per channel; the oldest leave first.
pub const MAX_KEPT: usize = 100;
/// Live changes kept per channel while its snapshot is pending; one more forces a resync.
pub const MAX_OVERLAY: usize = 128;

type Key = (u128, u32);

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
}

impl Channel {
    fn apply(&mut self, change: Change) {
        match change {
            Change::Delete(k) => {
                self.messages.remove(&k);
            }
            Change::Put { record, edited } => {
                let k = ts_key(&record.ts);
                if edited {
                    // An edit of a message not held is not added: it is older than the page, and a newer one
                    // missed would have been a gap.
                    if let Some(held) = self.messages.get_mut(&k) {
                        *held = record;
                    }
                    return;
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
            self.messages.pop_first();
        }
    }
}

/// The watched channels of one workspace and the socket gap they answer to. One per `Service`, made by
/// `Service::watches`.
pub struct Watches {
    /// The service's claim, released on drop; none for a model with no service (tests).
    claim: Option<Arc<AtomicBool>>,
    gap: Option<Gap>,
    /// The highest gap number seen. The gap moves forward only: a value read earlier, handed in later, is ignored.
    seen: u64,
    tickets: u64,
    channels: HashMap<String, Channel>,
}

impl Watches {
    pub(crate) fn claimed(claim: Arc<AtomicBool>) -> Watches {
        Watches { claim: Some(claim), gap: None, seen: 0, tickets: 0, channels: HashMap::new() }
    }

    #[cfg(test)]
    pub(crate) fn new() -> Watches {
        Watches { claim: None, gap: None, seen: 0, tickets: 0, channels: HashMap::new() }
    }

    pub(crate) fn claims(&self, claim: &Arc<AtomicBool>) -> bool {
        self.claim.as_ref().is_some_and(|c| Arc::ptr_eq(c, claim))
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
            let ch = Channel { phase: Phase::Needed, messages: BTreeMap::new() };
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

    /// The socket's gap as a caller read it. A gap newer than any seen makes every complete channel owe a snapshot,
    /// and one pending is refused when it is answered; an older value, read before a newer one was seen, changes nothing.
    pub(crate) fn sync(&mut self, current: Option<Gap>) {
        if let Some(g) = current.filter(|g| g.seq > self.seen) {
            self.seen = g.seq;
            self.gap = Some(g);
            for c in self.channels.values_mut() {
                if matches!(c.phase, Phase::Complete) {
                    c.phase = Phase::Needed;
                }
            }
        }
    }

    /// Begin a channel's snapshot, after the channel was registered. The service's pending events and gap are taken
    /// here and applied before the channel goes pending, so an event queued before the snapshot never wins over it and
    /// no order is left to the caller. The ticket goes back with the answer; `None` when the channel is
    /// unwatched or its snapshot is already pending or done (the events are applied either way).
    pub fn begin(&mut self, channel: &str, service: &Service) -> Option<u64> {
        self.begin_taken(channel, service.take(self))
    }

    /// The events pending in the socket, applied and the gap synced: what a caller does whenever `wait` says look,
    /// a snapshot pending or not. The take happens under this exclusive borrow, so no caller holds a taken event across a lock or an await while another
    /// applies newer ones.
    pub fn pump(&mut self, service: &Service) {
        self.apply_taken(service.take(self));
    }

    /// Wait at most `within` for something to take: an event queued or a gap latched, or anything still queued. True
    /// means look (`pump`) and may be spurious; false is a bound reached or the service stopped, a wait in flight
    /// included. The caller pumps after every wait. Taking `&mut self` is what makes one wait in flight the only
    /// wait: a second waiter would take the wake the pump needs.
    pub async fn wait(&mut self, service: &Service, within: Duration) -> bool {
        match service.waiter(self) {
            Some(w) => w.wait(within).await && service.running(),
            None => false,
        }
    }

    /// The service stopped: nothing held is current.
    fn ended(&mut self) {
        for c in self.channels.values_mut() {
            c.phase = Phase::Needed;
        }
    }

    pub(crate) fn apply_taken(&mut self, taken: Taken) {
        if taken.ended {
            self.ended();
        }
        self.sync(taken.gap);
        for event in &taken.events {
            self.apply(event);
        }
    }

    pub(crate) fn begin_taken(&mut self, channel: &str, taken: Taken) -> Option<u64> {
        self.apply_taken(taken);
        let read = self.gap;
        let c = self.channels.get_mut(channel).filter(|c| matches!(c.phase, Phase::Needed))?;
        self.tickets += 1;
        c.phase = Phase::Pending { ticket: self.tickets, read, overlay: Vec::new(), overflow: false };
        Some(self.tickets)
    }

    /// The snapshot is the channel's latest page; the service's gap is read as it arrives, so a
    /// loss since `begin` refuses it. Only its top-level messages of this channel are kept. The overlay is
    /// applied after it, so a live edit or delete wins over the snapshot's older content.
    pub fn loaded(&mut self, channel: &str, ticket: u64, records: Vec<ChatRecord>, service: &Service) -> Loaded {
        if !service.owns(self) {
            return Loaded::Stale;
        }
        let now = service.running_gap().unwrap_or_else(|| {
            self.ended();
            None
        });
        self.loaded_at(channel, ticket, records, now)
    }

    pub(crate) fn loaded_at(&mut self, channel: &str, ticket: u64, records: Vec<ChatRecord>, now: Option<Gap>) -> Loaded {
        self.sync(now);
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
        let top = |r: &ChatRecord| r.channel == channel && ts_key(&r.thread_ts) == ts_key(&r.ts);
        c.messages = records.into_iter().filter(top).map(|r| (ts_key(&r.ts), r)).collect();
        c.trim();
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
    pub(crate) fn apply(&mut self, event: &Normalized) -> bool {
        let (channel, change) = match event {
            Normalized::New(r) | Normalized::Edited(r) => {
                if ts_key(&r.thread_ts) != ts_key(&r.ts) {
                    return false;
                }
                let edited = matches!(event, Normalized::Edited(_));
                (&r.channel, Change::Put { record: r.clone(), edited })
            }
            Normalized::Deleted { channel, ts, .. } => (channel, Change::Delete(ts_key(ts))),
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

    /// Clear the socket's gap, through the service (`Socket::reconciled`), when every watch has resynced from a snapshot
    /// begun after that gap was read. `clear` refuses when something was lost since, and the gap stays.
    pub fn reconcile(&mut self, service: &Service) -> bool {
        self.reconcile_with(|g| service.reconciled(g))
    }

    pub(crate) fn reconcile_with(&mut self, clear: impl FnOnce(Gap) -> bool) -> bool {
        let Some(gap) = self.gap else { return false };
        if self.channels.values().all(|c| matches!(c.phase, Phase::Complete)) && clear(gap) {
            self.gap = None;
            return true;
        }
        false
    }
}

impl Drop for Watches {
    fn drop(&mut self) {
        if let Some(c) = &self.claim {
            c.store(false, Ordering::SeqCst);
        }
    }
}
