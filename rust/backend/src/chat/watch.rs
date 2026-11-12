//! The chat's watches in the daemon: which connection watches which channel (the table), the one owner task that holds
//! the service's one `Watches` and turns its changes into `chat.changed` and `chat.status` events, and the
//! connection's side (`Watcher`, whose drop releases its slots). A slot is a connection's watch of one channel and the
//! target it was admitted against. The owner calls Slack for one thing only, a channel's snapshot, and admits it again
//! at every attempt against the oldest watch whose row is still current; a watch whose row went away is released and
//! told to resync, so its refusal reaches it as the answer to its next `chat.watch`.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use sot_chat::api::{Bounds, HistoryQuery};
use sot_chat::delivery::{Action, Delivery, Filing, Timing, Verdict};
use sot_chat::mail::Filed;
use sot_chat::service::{Service, Status};
use sot_chat::traverse::{self, Order, Passed};
use sot_chat::watch::{Arrived, Loaded, State, Wake, Watches, MAX_CHANNELS, MAX_KEPT};
use sot_platform::host::env::Env;
use sot_protocol::chat::{ChatRecord, Ts};
use sot_protocol::ops::{
    ChatChangedEvt, ChatChannel, ChatCode, ChatService, ChatStatusEvt, ChatTarget, ChatWatchState, MAX_PAGE, MAX_PAGE_RECORDS,
};
use tokio::sync::{broadcast, mpsc, oneshot, Notify};
use tokio::task::JoinHandle;

use super::ops::{admitted, code_of};
use super::{mail, Chat};
use crate::comm::registry::registry::unix_now_secs;
use crate::rows::Workspaces;

/// The wait between a snapshot refused `busy` and its next attempt, and the attempts before the watch is told to resync.
const BUSY_PACE: Duration = Duration::from_millis(500);
const BUSY_ATTEMPTS: u32 = 8;
/// How long the owner waits for the socket before it looks again; only a bound, the wake is the socket's.
const LOOK: Duration = Duration::from_secs(30);
/// Events a connection may fall behind by before it is told it lost some (`bus.lost`).
const EVENTS_QUEUE: usize = 256;
/// How long a stop waits for the delivery's filing in flight (a name lookup and a guarded append, each bounded) before it
/// goes on; the retirement's own bound reports a longer wait and never extends the daemon's end.
const MAIL_END: Duration = Duration::from_secs(15);

/// One event for the connections that watch.
#[derive(Clone)]
pub(crate) enum Evt {
    /// Written to a connection that watches the channel when it reaches the loop.
    Changed(ChatChangedEvt),
    /// One entry for each connection the status concerns, written to that connection alone.
    Status(Arc<Vec<(u64, ChatStatusEvt)>>),
}

#[derive(Default)]
struct Table {
    /// At most one per connection and channel.
    slots: Vec<(u64, ChatTarget)>,
    /// What the owner last said of each watched channel.
    states: BTreeMap<String, ChatWatchState>,
}

impl Table {
    fn channels(&self) -> BTreeSet<String> {
        self.slots.iter().map(|(_, t)| t.channel.as_str().to_string()).collect()
    }

    fn holds(&self, conn: u64, channel: &str) -> bool {
        self.slots.iter().any(|(c, t)| *c == conn && t.channel.as_str() == channel)
    }

    /// One entry for each connection with a slot: its channels among `of`.
    fn status(&self, service: ChatService, gap: bool, of: &BTreeSet<String>) -> Vec<(u64, ChatStatusEvt)> {
        let conns: BTreeSet<u64> = self.slots.iter().map(|(c, _)| *c).collect();
        let mine = |conn: u64| -> Vec<ChatChannel> {
            self.slots.iter().filter(|(c, t)| *c == conn && of.contains(t.channel.as_str())).map(|(_, t)| t.channel.clone()).collect()
        };
        conns.into_iter().map(|c| (c, ChatStatusEvt { service, gap, resync: mine(c) })).collect()
    }
}

enum Owner {
    Idle,
    Running,
    /// The service stopped: this daemon's chat does not start again.
    Ended,
}

pub(crate) struct Hub {
    table: Mutex<Table>,
    kick: Notify,
    events: broadcast::Sender<Evt>,
    owner: Mutex<Owner>,
    conns: AtomicU64,
    /// The stop's way to the owner: a request to end the delivery, carrying the answer's sender. `None` until an owner
    /// runs; a send that fails, or an answer dropped unsent, means no owner is left to wait for.
    ending: Mutex<Option<mpsc::UnboundedSender<oneshot::Sender<()>>>>,
    /// The delivery's timings, read when the owner starts.
    timing: Mutex<Timing>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hub {
    pub(crate) fn new() -> Arc<Hub> {
        Arc::new(Hub {
            table: Mutex::new(Table::default()),
            kick: Notify::new(),
            events: broadcast::channel(EVENTS_QUEUE).0,
            owner: Mutex::new(Owner::Idle),
            conns: AtomicU64::new(0),
            ending: Mutex::new(None),
            timing: Mutex::new(Timing::PRODUCT),
        })
    }

    /// The delivery's timings, read when the owner starts.
    pub(super) fn set_timing(&self, timing: Timing) {
        *lock(&self.timing) = timing;
    }

    /// Ask a running owner to end its delivery and wait until it has (bounded): the stop calls it before the service's, so
    /// the lock outlives the last filing.
    pub(super) async fn end_mail(&self) {
        let Some(ask) = lock(&self.ending).clone() else { return };
        let (done, over) = oneshot::channel();
        if ask.send(done).is_ok() {
            let _ = tokio::time::timeout(MAIL_END, over).await;
        }
    }

    /// A connection's side, subscribed before its first request is read.
    pub(crate) fn connect(self: &Arc<Hub>) -> Watcher {
        let conn = Conn { hub: self.clone(), id: self.conns.fetch_add(1, Ordering::Relaxed) };
        Watcher { rx: self.events.subscribe(), conn }
    }

    #[cfg(test)]
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Evt> {
        self.events.subscribe()
    }

    /// The held slots as (connection, channel).
    #[cfg(test)]
    pub(crate) fn slots(&self) -> Vec<(u64, String)> {
        lock(&self.table).slots.iter().map(|(c, t)| (*c, t.channel.as_str().to_string())).collect()
    }

    #[cfg(test)]
    pub(crate) fn ended(&self) -> bool {
        matches!(*lock(&self.owner), Owner::Ended)
    }

    /// Whether the owner is running.
    #[cfg(test)]
    pub(crate) fn running(&self) -> bool {
        matches!(*lock(&self.owner), Owner::Running)
    }

    /// A service another `Watches` already serves is refused.
    pub(super) fn ensure_owner(self: &Arc<Hub>, service: &Arc<Service>, chat: &Arc<Chat>, rows: &Workspaces) -> Result<(), ChatCode> {
        let mut owner = lock(&self.owner);
        match *owner {
            Owner::Running => Ok(()),
            Owner::Ended => Err(ChatCode::Unavailable),
            Owner::Idle => {
                let watches = service.watches().ok_or(ChatCode::Unavailable)?;
                *owner = Owner::Running;
                let own = Own { hub: self.clone(), service: service.clone(), chat: chat.clone(), rows: rows.clone(), known: BTreeSet::new(), told: BTreeSet::new(), mail: None };
                let (ask, ending) = mpsc::unbounded_channel();
                *lock(&self.ending) = Some(ask);
                tokio::spawn(own.run(watches, ending));
                Ok(())
            }
        }
    }

    fn send(&self, evt: Evt) {
        // No receiver is no connection to tell.
        let _ = self.events.send(evt);
    }
}

/// A connection's identity for watches: cheap to copy into a handler.
#[derive(Clone)]
pub(crate) struct Conn {
    hub: Arc<Hub>,
    id: u64,
}

impl Conn {
    /// Watch `target`'s channel. The daemon holds at most `MAX_CHANNELS` channels in all (the model's bound, which wire's `MAX_WATCHED` repeats), so a
    /// connection, whose slots are among them, holds at most that many too; a connection may join a channel another
    /// holds, and a watch it already holds is replaced by this one.
    pub(crate) fn claim(&self, target: ChatTarget) -> Result<(), ChatCode> {
        let mut table = lock(&self.hub.table);
        let channel = target.channel.as_str();
        let held = table.channels();
        if held.len() >= MAX_CHANNELS && !held.contains(channel) {
            return Err(ChatCode::TooManyWatches);
        }
        table.slots.retain(|(c, t)| !(*c == self.id && t.channel.as_str() == channel));
        table.slots.push((self.id, target));
        Ok(())
    }

    /// End this connection's watch of the channel, and no other's.
    pub(crate) fn release(&self, channel: &str) {
        lock(&self.hub.table).slots.retain(|(c, t)| !(*c == self.id && t.channel.as_str() == channel));
        self.hub.kick.notify_one();
    }

    pub(crate) fn is(&self, id: u64) -> bool {
        self.id == id
    }

    pub(crate) fn watches(&self, channel: &str) -> bool {
        lock(&self.hub.table).holds(self.id, channel)
    }

    /// The state to answer a watch with: complete when the owner holds the channel's snapshot, else a snapshot is under
    /// way or about to begin. Never `needed`: a channel that owes one is told by `chat.status`.
    pub(crate) fn state(&self, channel: &str) -> ChatWatchState {
        match lock(&self.hub.table).states.get(channel) {
            Some(ChatWatchState::Complete) => ChatWatchState::Complete,
            _ => ChatWatchState::Pending,
        }
    }

    /// Register the watch, start the owner if it is not running and have it begin the snapshot.
    pub(crate) async fn watch(&self, target: ChatTarget, service: &Arc<Service>, chat: &Arc<Chat>, rows: &Workspaces) -> Result<ChatWatchState, ChatCode> {
        let channel = target.channel.as_str().to_string();
        self.claim(target)?;
        if let Err(code) = self.hub.ensure_owner(service, chat, rows) {
            self.release(&channel);
            return Err(code);
        }
        self.hub.kick.notify_one();
        // A test's hold point, keyed by the hub's address, between the registration and the reply. The owner is kicked
        // above, so its snapshot runs while this is held; `chat.watch.snapshot` holds that.
        sot_platform::hold_point_async!("chat.watch.between", Arc::as_ptr(&self.hub) as usize);
        Ok(self.state(&channel))
    }
}

/// A connection's receiver and, by its drop, its end: every slot it holds is released, whatever ended the connection.
pub(crate) struct Watcher {
    pub(crate) rx: broadcast::Receiver<Evt>,
    pub(crate) conn: Conn,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        lock(&self.conn.hub.table).slots.retain(|(c, _)| *c != self.conn.id);
        self.conn.hub.kick.notify_one();
    }
}

/// What the snapshot task hands back.
struct Fetched {
    channel: String,
    ticket: u64,
    /// Connections whose watch of the channel was refused at admission.
    stale: Vec<u64>,
    result: Result<Vec<ChatRecord>, ChatCode>,
}

struct Own {
    hub: Arc<Hub>,
    service: Arc<Service>,
    chat: Arc<Chat>,
    rows: Workspaces,
    /// The channels the model watches.
    known: BTreeSet<String>,
    /// The channels last told to resync.
    told: BTreeSet<String>,
    /// The delivery, when the hub's `chat.toml` maps a delivering channel.
    mail: Option<Mail>,
}

/// What a job of the delivery hands back to the owner.
enum Done {
    /// The start's read of the inboxes.
    Read(Vec<Filed>),
    Filed { channel: String, ts: Ts, live: bool, result: Result<(), String> },
    Passed(String, Passed),
    /// The pass was not made, for a fixed code (no recipient, a stopped service, a job that did not finish).
    Skipped(String, &'static str),
}

/// The pure `Delivery` and what its owner keeps to run it: each job's handle and the instant to ask again.
struct Mail {
    delivery: Delivery,
    workspace: String,
    /// The state folder, on this computer's disk.
    dir: std::path::PathBuf,
    /// Channel and its mapped folder.
    folders: Vec<(String, String)>,
    tx: mpsc::Sender<Done>,
    wake: Option<tokio::time::Instant>,
    /// The start's read, the filing in flight and the pass in flight.
    read: JoinHandle<()>,
    filing: Option<JoinHandle<()>>,
    pass: Option<JoinHandle<()>>,
}

impl Drop for Mail {
    fn drop(&mut self) {
        self.read.abort();
        self.pass.iter().for_each(JoinHandle::abort);
    }
}

/// The start's read of every inbox, tried again on the backoff while it fails; nothing is filed or passed before it succeeds.
async fn read_inboxes(env: Env, tx: mpsc::Sender<Done>, timing: Timing) {
    let mut wait = timing.backoff_first;
    while !tx.is_closed() {
        let e = env.clone();
        match tokio::task::spawn_blocking(move || mail::read_filings(&e)).await {
            Ok(Ok(filings)) => return drop(tx.send(Done::Read(filings)).await),
            Ok(Err(why)) => tracing::warn!(why, "project chat: the inboxes could not be read; nothing is filed until they are"),
            Err(_) => tracing::warn!("project chat: the read of the inboxes did not finish"),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(timing.backoff_max);
    }
}

/// The `ts` values whose filing job panics, so a test can prove a failed job still answers.
#[cfg(test)]
pub(crate) static PANIC_ON: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// One filing: the author named as a read names it, then the guarded append.
async fn file_job(env: Env, rows: Workspaces, service: Arc<Service>, folder: String, mut f: Filing) -> Done {
    #[cfg(test)]
    assert!(!PANIC_ON.lock().unwrap().contains(&f.record.ts.as_str().to_string()), "a test's filing job fails here");
    sot_platform::hold_point_async!("chat.mail.filing", folder);
    if let Ok(api) = service.api() {
        api.fill_names(std::slice::from_mut(&mut f.record)).await;
    }
    Done::Filed { result: mail::file(&env, &rows, &folder, &f.record).await, channel: f.channel, ts: f.record.ts, live: f.live }
}

/// The mapped folders whose pass job panics, so a test can prove a failed pass still answers.
#[cfg(test)]
pub(crate) static PANIC_PASS_IN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// One pass: the recipient first (an absent one costs no read), then the walk to the clock's reading now.
async fn pass_job(env: Env, rows: Workspaces, service: Arc<Service>, folder: String, order: Order, pace: Duration) -> Done {
    #[cfg(test)]
    assert!(!PANIC_PASS_IN.lock().unwrap().contains(&folder), "a test's pass job fails here");
    sot_platform::hold_point_async!("chat.mail.pass", folder);
    let channel = order.state.channel().to_string();
    match tokio::task::spawn_blocking(move || mail::recipient(&rows, &folder, &env, unix_now_secs())).await {
        Ok(Ok(_)) => {}
        Ok(Err(code)) => return Done::Skipped(channel, code),
        Err(_) => return Done::Skipped(channel, "recipient_failed"),
    }
    let (Ok(api), Ok(now)) = (service.api(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)) else { return Done::Skipped(channel, "stopped") };
    let upper = Ts::parse(&format!("{}.{:06}", now.as_secs(), now.subsec_micros())).expect("seconds and six digits are a timestamp");
    Done::Passed(channel, traverse::pass(&api, &order, &upper, pace).await)
}

/// Ends a task when it is dropped, so the job a `run_job` task waits on goes with it.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs `job` on a task of its own and sends its answer; a job that panics or is cancelled answers `failed`, so the
/// delivery's one filing or pass in flight always returns. Aborting the returned task ends the job with it.
fn run_job(tx: mpsc::Sender<Done>, job: impl Future<Output = Done> + Send + 'static, failed: Done) -> JoinHandle<()> {
    tokio::spawn(async move {
        let job = tokio::spawn(job);
        let _ends_with_us = AbortOnDrop(job.abort_handle());
        let done = job.await.unwrap_or_else(|e| {
            tracing::warn!(cancelled = e.is_cancelled(), "project chat: a delivery job did not finish");
            failed
        });
        drop(tx.send(done).await)
    })
}

fn held(w: &Watches, channel: &str) -> Vec<ChatRecord> {
    w.messages(channel).into_iter().cloned().collect()
}

/// What changed from `old` to `new`, both oldest first: the records new or altered and the timestamps gone. At capacity
/// the model drops its oldest, which Slack still holds, so a timestamp older than every one held is not reported gone.
fn diff(old: &[ChatRecord], new: &[ChatRecord]) -> (Vec<ChatRecord>, Vec<Ts>) {
    let put = new.iter().filter(|r| old.iter().find(|o| o.ts == r.ts) != Some(r)).cloned().collect();
    let floor = if new.len() >= MAX_KEPT { new.first().map(|r| r.ts.as_str().to_string()) } else { None };
    let gone = old
        .iter()
        .filter(|o| !new.iter().any(|r| r.ts == o.ts))
        .filter(|o| floor.as_deref().is_none_or(|f| (o.ts.as_str().len(), o.ts.as_str()) > (f.len(), f)))
        .map(|o| o.ts.clone())
        .collect();
    (put, gone)
}

/// `attempt`, again after `pace` while it is refused `busy`, at most `tries` times: a busy snapshot is a full HTTP
/// core (shared with the listener), not a stale watch. Any other result is final.
async fn paced<T, F, Fut>(pace: Duration, tries: u32, mut attempt: F) -> Result<T, ChatCode>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ChatCode>>,
{
    for _ in 1..tries {
        match attempt().await {
            Err(ChatCode::Busy) => tokio::time::sleep(pace).await,
            done => return done,
        }
    }
    attempt().await
}

/// One snapshot attempt: every watch is admitted, the call runs under the oldest whose row is current, and every watch
/// refused for its row is named in `stale`; then the channel's latest page.
pub(crate) async fn snapshot(chat: &Arc<Chat>, rows: &Workspaces, targets: &[(u64, ChatTarget)], channel: &str, stale: &Mutex<BTreeSet<u64>>) -> Result<Vec<ChatRecord>, ChatCode> {
    let mut found = Err(ChatCode::Unavailable);
    for (conn, target) in targets {
        let admission = admitted(chat, rows, target.clone()).await;
        if let Err(ChatCode::StaleRow | ChatCode::StaleMapping | ChatCode::Unmapped) = &admission {
            lock(stale).insert(*conn);
        }
        // The call runs under the first row that is current; every watch is admitted, so every stale one is named.
        if found.is_err() {
            found = admission;
        }
    }
    let api = found?.api().map_err(|_| ChatCode::Unavailable)?;
    let query = Bounds::new(None, None, None).and_then(|b| HistoryQuery::new(channel, None, b, MAX_PAGE)).map_err(|e| code_of(&e))?;
    api.history(&query, None).await.map(|p| p.records).map_err(|e| code_of(&e))
}

impl Own {
    async fn run(mut self, mut watches: Watches, mut ending: mpsc::UnboundedReceiver<oneshot::Sender<()>>) {
        let (done_tx, mut done_rx) = mpsc::channel::<Fetched>(MAX_CHANNELS);
        let (mail_tx, mut mail_rx) = mpsc::channel::<Done>(8);
        self.mail = self.open_mail(&mut watches, mail_tx);
        loop {
            let at = self.mail.as_ref().and_then(|m| m.wake);
            tokio::select! {
                _ = self.hub.kick.notified() => self.sync(&mut watches, &done_tx),
                Some(f) = done_rx.recv() => self.finish(&mut watches, f),
                Some(d) = mail_rx.recv() => self.returned(d),
                Some(done) = ending.recv() => self.end_mail(done).await,
                _ = async { match at { Some(t) => tokio::time::sleep_until(t).await, None => std::future::pending().await } } => {}
                wake = watches.wait(&self.service, LOOK) => match wake {
                    Wake::Stopped => break,
                    Wake::Look => {
                        let service = self.service.clone();
                        let arrived = self.step(&mut watches, None, |w| w.pump(&service));
                        self.hand_over(arrived);
                    }
                    Wake::Idle => {}
                },
            }
            self.publish(&mut watches);
            self.drive();
        }
        self.end();
    }

    fn hand_over(&mut self, arrived: Arrived) {
        if let Some(m) = &mut self.mail {
            m.delivery.hand_over(arrived);
        }
    }

    /// The delivery for the hub's delivering channels, whose states are read and whose records the model hands over from
    /// here on; the start's read of the inboxes begins. A channel whose state cannot be read stays off.
    fn open_mail(&self, watches: &mut Watches, tx: mpsc::Sender<Done>) -> Option<Mail> {
        let Some(Ok(config)) = &self.chat.config else { return None };
        let dir = sot_platform::host::state_dir::daemon_state_dir_in(&self.chat.env).join("chat");
        let (mut states, mut folders) = (Vec::new(), Vec::new());
        for (folder, channel) in config.mappings() {
            let Some(floor) = config.floor(channel) else { continue };
            match sot_chat::state::State::load(&dir, config.workspace(), channel, &floor) {
                Ok(state) if watches.deliver(channel).is_ok() => {
                    states.push((state, floor));
                    folders.push((channel.to_string(), folder.to_string()));
                }
                Ok(_) => {}
                Err(code) => tracing::warn!(?code, channel, "project chat: a delivering channel's state is unreadable; it stays off"),
            }
        }
        if states.is_empty() {
            return None;
        }
        let timing = *lock(&self.hub.timing);
        let read = tokio::spawn(read_inboxes(self.chat.env.clone(), tx.clone(), timing));
        Some(Mail { delivery: Delivery::new(timing, std::time::Instant::now(), states), workspace: config.workspace().into(), dir, folders, tx, wake: None, read, filing: None, pass: None })
    }

    /// A job came back: the delivery learns its result, and the daemon's log gets one line (fixed codes and ids, no text).
    fn returned(&mut self, done: Done) {
        let Some(m) = &mut self.mail else { return };
        let now = std::time::Instant::now();
        let over = match done {
            Done::Read(filings) => {
                for (channel, _) in &m.folders {
                    tracing::info!(channel, filings = filings.iter().filter(|f| &f.channel == channel).count(), "project chat: the inboxes were read");
                }
                m.delivery.start(&filings)
            }
            Done::Filed { channel, ts, live, result } => {
                let reason = result.as_ref().err().map(|e| e.split(':').next().unwrap_or_default().to_string());
                tracing::info!(channel, ts = ts.as_str(), source = if live { "live" } else { "pass" }, filed = result.is_ok(), reason, "project chat: mail");
                m.delivery.filed(now, if result.is_ok() { Verdict::Filed } else { Verdict::Refused })
            }
            Done::Passed(channel, p) => {
                tracing::info!(channel, outcome = ?p.outcome, reads = p.reads, mail = p.mail.len(), limited = p.limited, skipped = p.skipped, "project chat: pass");
                m.delivery.passed(now, p);
                false
            }
            Done::Skipped(channel, code) => {
                tracing::info!(channel, code, "project chat: pass skipped");
                m.delivery.pass_skipped(now);
                false
            }
        };
        if over {
            tracing::warn!("project chat: a channel's filed set passed its bound");
        }
    }

    /// The stop's first step: the delivery ends. Its filing in flight returns (bounded), its pass is dropped (its reads end
    /// with the service's API, which the stop closes next) and so is the start's read; then the stop is told.
    async fn end_mail(&mut self, done: oneshot::Sender<()>) {
        if let Some(filing) = self.mail.take().and_then(|mut m| m.filing.take()) {
            let _ = tokio::time::timeout(MAIL_END, filing).await;
        }
        drop(done.send(()));
    }

    /// Ask the delivery what to do next and start it, until it has nothing to do now. A commit is not answered: the
    /// delivery has moved its boundary in memory, a write that fails leaves the old file (a stale file only costs the next
    /// start reads), and the task that writes it logs the failure.
    fn drive(&mut self) {
        let Some(m) = &mut self.mail else { return };
        loop {
            let folder = |channel: &str| m.folders.iter().find(|(c, _)| c == channel).map(|(_, folder)| folder.clone()).unwrap_or_default();
            let (env, rows, service, tx) = (self.chat.env.clone(), self.rows.clone(), self.service.clone(), m.tx.clone());
            match m.delivery.poll(std::time::Instant::now()) {
                Action::File(f) => {
                    let (folder, failed) = (folder(&f.channel), Done::Filed { channel: f.channel.clone(), ts: f.record.ts.clone(), live: f.live, result: Err("job_failed".into()) });
                    m.filing = Some(run_job(tx, file_job(env, rows, service, folder, f), failed));
                }
                Action::Pass { order, pace } => {
                    let channel = order.state.channel().to_string();
                    m.pass = Some(run_job(tx, pass_job(env, rows, service, folder(&channel), order, pace), Done::Skipped(channel, "job_failed")));
                }
                Action::Commit { channel, boundary } => {
                    let (dir, workspace) = (m.dir.clone(), m.workspace.clone());
                    drop(tokio::task::spawn_blocking(move || {
                        if let Err(code) = mail::commit(&dir, &workspace, &channel, &boundary) {
                            tracing::warn!(?code, channel, "project chat: a boundary was not written");
                        }
                    }));
                }
                Action::Idle(at) => return m.wake = at.map(tokio::time::Instant::from_std),
            }
        }
    }

    /// Run `change` on the model and tell the watchers what it changed. `base` replaces one channel's before-picture
    /// (a snapshot is not an event: only what came after it is).
    fn step<R>(&self, watches: &mut Watches, base: Option<(&str, Vec<ChatRecord>)>, change: impl FnOnce(&mut Watches) -> R) -> R {
        let mut before: BTreeMap<String, Vec<ChatRecord>> = self.known.iter().map(|c| (c.clone(), held(watches, c))).collect();
        let out = change(watches);
        if let Some((channel, records)) = base {
            before.insert(channel.to_string(), records);
        }
        for (channel, old) in before {
            let (mut put, gone) = diff(&old, &held(watches, &channel));
            // A change event names people from the cache alone: a name a history read has looked up, never a request.
            if let Ok(api) = self.service.api() {
                api.cached_names(&mut put);
            }
            let Some(workspace) = lock(&self.hub.table).slots.iter().find(|(_, t)| t.channel.as_str() == channel).map(|(_, t)| t.workspace.clone()) else {
                continue;
            };
            let Ok(chan) = ChatChannel::try_from(channel) else { continue };
            let (mut put, mut gone) = (put.chunks(MAX_PAGE_RECORDS), gone.chunks(MAX_PAGE_RECORDS));
            loop {
                let (p, g) = (put.next(), gone.next());
                if p.is_none() && g.is_none() {
                    break;
                }
                let evt = ChatChangedEvt { workspace: workspace.clone(), channel: chan.clone(), put: p.unwrap_or_default().to_vec(), deleted: g.unwrap_or_default().to_vec() };
                self.hub.send(Evt::Changed(evt));
            }
        }
        out
    }

    /// Bring the model to the table: watch what is newly watched, forget what no connection holds, and begin the
    /// snapshot of every watched channel that owes one.
    fn sync(&mut self, watches: &mut Watches, done: &mpsc::Sender<Fetched>) {
        let want = lock(&self.hub.table).channels();
        for gone in self.known.difference(&want).cloned().collect::<Vec<_>>() {
            watches.unwatch(&gone);
            self.known.remove(&gone);
        }
        for channel in want {
            if self.known.insert(channel.clone()) && watches.watch(&channel).is_err() {
                self.known.remove(&channel);
                continue;
            }
            if watches.state(&channel) != Some(State::Needed) {
                continue;
            }
            let service = self.service.clone();
            let (ticket, arrived) = self.step(watches, None, |w| w.begin(&channel, &service));
            self.hand_over(arrived);
            let Some(ticket) = ticket else { continue };
            let targets: Vec<(u64, ChatTarget)> = lock(&self.hub.table).slots.iter().filter(|(_, t)| t.channel.as_str() == channel).cloned().collect();
            let (chat, rows, done) = (self.chat.clone(), self.rows.clone(), done.clone());
            let key = Arc::as_ptr(&self.hub) as usize;
            tokio::spawn(async move {
                // A test's hold point, keyed by the hub's address, before the snapshot's first admission.
                sot_platform::hold_point_async!("chat.watch.snapshot", key);
                let stale = Mutex::new(BTreeSet::new());
                let result = paced(BUSY_PACE, BUSY_ATTEMPTS, || snapshot(&chat, &rows, &targets, &channel, &stale)).await;
                let stale = lock(&stale).iter().copied().collect();
                let _ = done.send(Fetched { channel, ticket, stale, result }).await;
            });
        }
    }

    fn finish(&mut self, watches: &mut Watches, f: Fetched) {
        for conn in &f.stale {
            lock(&self.hub.table).slots.retain(|(c, t)| !(c == conn && t.channel.as_str() == f.channel));
        }
        let gap = self.service.gap().is_some();
        let released = f.stale.iter().map(|c| (*c, ChatStatusEvt { service: ChatService::Running, gap, resync: ChatChannel::try_from(f.channel.clone()).into_iter().collect() })).collect();
        if !f.stale.is_empty() {
            self.hub.send(Evt::Status(Arc::new(released)));
        }
        match f.result {
            Ok(records) => {
                let top: Vec<ChatRecord> = records.iter().filter(|r| r.channel == f.channel && r.thread_ts == r.ts).cloned().collect();
                let service = self.service.clone();
                let loaded = self.step(watches, Some((&f.channel, top)), |w| w.loaded(&f.channel, f.ticket, records, &service));
                if loaded == Loaded::Complete {
                    watches.reconcile(&self.service);
                }
            }
            Err(_) => watches.failed(&f.channel, f.ticket),
        }
    }

    /// Say what the table should show of the model: each channel's state, and the channels that owe a snapshot, told
    /// once each time the set changes.
    fn publish(&mut self, watches: &Watches) {
        let mut needed = BTreeSet::new();
        let mut table = lock(&self.hub.table);
        table.states.clear();
        for channel in &self.known {
            let state = match watches.state(channel) {
                Some(State::Complete) => ChatWatchState::Complete,
                Some(State::Pending) => ChatWatchState::Pending,
                _ => {
                    needed.insert(channel.clone());
                    ChatWatchState::Needed
                }
            };
            table.states.insert(channel.clone(), state);
        }
        // A service that is stopping makes every channel read as owing a snapshot: `end` says so once, as the stop. The
        // status is read after the states: a stop sets its phase before it ends the claim they read.
        if needed == self.told || !matches!(self.service.status(), Status::Running { .. }) {
            return;
        }
        self.told = needed.clone();
        if !needed.is_empty() {
            let status = table.status(ChatService::Running, self.service.gap().is_some(), &needed);
            drop(table);
            self.hub.send(Evt::Status(Arc::new(status)));
        }
    }

    /// The service stopped: every watching connection is told its channels need a snapshot, and every watch ends.
    fn end(self) {
        let phase = match self.service.status() {
            Status::Stopping => ChatService::Stopping,
            _ => ChatService::Stopped,
        };
        let mut table = lock(&self.hub.table);
        let all = table.channels();
        let status = table.status(phase, false, &all);
        table.slots.clear();
        table.states.clear();
        drop(table);
        self.hub.send(Evt::Status(Arc::new(status)));
        *lock(&self.hub.owner) = Owner::Ended;
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
