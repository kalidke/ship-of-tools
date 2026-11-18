//! The chat's watches in the daemon: which connection watches which channel of which workspace (the table, keyed by the
//! pair), one owner task per workspace (its desk) that holds that workspace's service's one `Watches` and turns its
//! changes into `chat.changed` and `chat.status` events, the daemon's one filing slot, and the connection's side
//! (`Watcher`, whose drop releases its slots). A slot is a connection's watch of one channel and the target it was
//! admitted against. The owner calls Slack for one thing only, a channel's snapshot, and admits it again
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

use super::ops::{admitted, code_of, read_code};
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
pub(crate) const MAIL_END: Duration = Duration::from_secs(15);

/// One event for the connections that watch.
#[derive(Clone)]
pub(crate) enum Evt {
    /// Written to a connection that watches the channel when it reaches the loop.
    Changed(ChatChangedEvt),
    /// One entry for each connection the status concerns, written to that connection alone.
    Status(Arc<Vec<(u64, ChatStatusEvt)>>),
}

/// A slot's workspace and channel.
fn pair(t: &ChatTarget) -> (&str, &str) {
    (t.workspace.as_str(), t.channel.as_str())
}

#[derive(Default)]
struct Table {
    /// At most one per connection, workspace and channel.
    slots: Vec<(u64, ChatTarget)>,
    /// What each workspace's owner last said of each of its watched channels, by (workspace, channel).
    states: BTreeMap<(String, String), ChatWatchState>,
    /// The watched channels whose last snapshot Slack refused because the app is not in them (`not_in_channel`), kept
    /// until a snapshot succeeds or the channel is no longer watched: a watch of one is answered with that refusal.
    refused: BTreeMap<(String, String), ChatCode>,
}

impl Table {
    /// The channels watched in `workspace`.
    fn channels_of(&self, workspace: &str) -> BTreeSet<String> {
        self.slots.iter().filter(|(_, t)| t.workspace == workspace).map(|(_, t)| t.channel.as_str().to_string()).collect()
    }

    /// Every watched (workspace, channel), whatever the workspace: the watch bound counts them all.
    fn pairs(&self) -> BTreeSet<(String, String)> {
        self.slots.iter().map(|(_, t)| (t.workspace.clone(), t.channel.as_str().to_string())).collect()
    }

    fn holds(&self, conn: u64, workspace: &str, channel: &str) -> bool {
        self.slots.iter().any(|(c, t)| *c == conn && pair(t) == (workspace, channel))
    }

    /// One entry for each connection with a slot in `workspace`: its channels of that workspace among `of`.
    fn status(&self, workspace: &str, service: ChatService, gap: bool, of: &BTreeSet<String>) -> Vec<(u64, ChatStatusEvt)> {
        let conns: BTreeSet<u64> = self.slots.iter().filter(|(_, t)| t.workspace == workspace).map(|(c, _)| *c).collect();
        let mine = |conn: u64| -> Vec<ChatChannel> {
            self.slots
                .iter()
                .filter(|(c, t)| *c == conn && t.workspace == workspace && of.contains(t.channel.as_str()))
                .map(|(_, t)| t.channel.clone())
                .collect()
        };
        conns.into_iter().map(|c| (c, ChatStatusEvt { service, gap, resync: mine(c) })).collect()
    }

    /// `workspace`'s end: each connection with a slot in it is told its channels there owe a snapshot, and its slots and
    /// states go; every other workspace's stay.
    fn end_of(&mut self, workspace: &str, phase: ChatService) -> Vec<(u64, ChatStatusEvt)> {
        let all = self.channels_of(workspace);
        let status = self.status(workspace, phase, false, &all);
        self.slots.retain(|(_, t)| t.workspace != workspace);
        self.states.retain(|(w, _), _| w != workspace);
        self.refused.retain(|(w, _), _| w != workspace);
        status
    }
}

/// One workspace's owner: whether it runs, its kick (a `Notify` wakes one waiter, so each owner has its own), and the
/// stop's way to it.
#[derive(Default)]
struct Desk {
    /// No owner runs while false: none started yet, or the last one's service ended, so the workspace's next running
    /// service starts one.
    running: bool,
    kick: Arc<Notify>,
    /// The stop's way to the owner: a request to end the delivery, carrying the answer's sender. `None` until an owner
    /// runs; a send that fails, or an answer dropped unsent, means no owner is left to wait for.
    ending: Option<mpsc::UnboundedSender<oneshot::Sender<()>>>,
}

/// The daemon's one filing slot and its one `FILE_GAP` clock (C8): every workspace's filing job takes the slot, waits
/// until `FILE_GAP` after the daemon's last filing returned, files, records its return and frees the slot, so a registry
/// writer of another process meets free polls after every filing's hold whatever the number of workspaces.
struct FilingSlot {
    one: tokio::sync::Semaphore,
    returned: Mutex<Option<tokio::time::Instant>>,
}

impl FilingSlot {
    fn new() -> FilingSlot {
        FilingSlot { one: tokio::sync::Semaphore::new(1), returned: Mutex::new(None) }
    }

    /// `file` run at the slot: the slot taken, then `FILE_GAP` after the daemon's last filing returned, `file`, and its
    /// return recorded as the slot is freed. `None`, with nothing run, for a job its delivery's end dropped while it
    /// waited (`at`).
    async fn file<T>(&self, at: &Mutex<Slot>, file: impl Future<Output = T>) -> Option<T> {
        let _held = self.one.acquire().await.ok()?;
        {
            let mut at = lock(at);
            if *at == Slot::Dropped {
                return None;
            }
            *at = Slot::Holding;
        }
        let after = *lock(&self.returned);
        if let Some(after) = after {
            tokio::time::sleep_until(after + sot_chat::delivery::FILE_GAP).await;
        }
        let out = file.await;
        *lock(&self.returned) = Some(tokio::time::Instant::now());
        Some(out)
    }
}

pub(crate) struct Hub {
    table: Mutex<Table>,
    events: broadcast::Sender<Evt>,
    /// Each workspace's owner, by workspace.
    desks: Mutex<BTreeMap<String, Desk>>,
    /// Woken when any owner ends.
    idle: Notify,
    conns: AtomicU64,
    /// The delivery's timings, read when an owner starts.
    timing: Mutex<Timing>,
    filing: Arc<FilingSlot>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hub {
    pub(crate) fn new() -> Arc<Hub> {
        Arc::new(Hub {
            table: Mutex::new(Table::default()),
            events: broadcast::channel(EVENTS_QUEUE).0,
            desks: Mutex::new(BTreeMap::new()),
            idle: Notify::new(),
            conns: AtomicU64::new(0),
            timing: Mutex::new(Timing::PRODUCT),
            filing: Arc::new(FilingSlot::new()),
        })
    }

    /// The delivery's timings, read when the owner starts.
    pub(super) fn set_timing(&self, timing: Timing) {
        *lock(&self.timing) = timing;
    }

    /// Wake `workspace`'s owner, when it has a desk (an owner ran or runs): a kick is kept until that owner waits. A
    /// workspace no owner ever ran has nothing to wake (its first owner reads the table at its start), and gets no desk.
    fn kick(&self, workspace: &str) {
        if let Some(d) = lock(&self.desks).get(workspace) {
            d.kick.notify_one();
        }
    }

    /// Wake every workspace's owner.
    fn kick_all(&self) {
        lock(&self.desks).values().for_each(|d| d.kick.notify_one());
    }

    /// Ask `workspace`'s running owner to end its delivery and wait until it has (bounded): the stop calls it before the
    /// service's, so the lock outlives the last filing.
    pub(super) async fn end_mail(&self, workspace: &str) {
        let Some(ask) = lock(&self.desks).get(workspace).and_then(|d| d.ending.clone()) else { return };
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

    /// The workspaces with a desk.
    #[cfg(test)]
    pub(crate) fn desks(&self) -> Vec<String> {
        lock(&self.desks).keys().cloned().collect()
    }

    /// The held slots as (connection, channel).
    #[cfg(test)]
    pub(crate) fn slots(&self) -> Vec<(u64, String)> {
        lock(&self.table).slots.iter().map(|(c, t)| (*c, t.channel.as_str().to_string())).collect()
    }

    /// Whether any workspace's owner is running.
    #[cfg(test)]
    pub(crate) fn running(&self) -> bool {
        lock(&self.desks).values().any(|d| d.running)
    }

    /// Whether `workspace`'s owner is running.
    #[cfg(test)]
    pub(crate) fn running_in(&self, workspace: &str) -> bool {
        lock(&self.desks).get(workspace).is_some_and(|d| d.running)
    }

    /// Until `workspace`'s owner is not running: a reload starts its service only after the last one's owner ended, or the
    /// new service's `ensure_owner` would find the old owner running and start none.
    pub(super) async fn owner_ended(&self, workspace: &str) {
        loop {
            let ended = self.idle.notified();
            tokio::pin!(ended);
            ended.as_mut().enable();
            if !lock(&self.desks).get(workspace).is_some_and(|d| d.running) {
                return;
            }
            ended.await;
        }
    }

    /// The owner of `service`'s workspace, started unless it runs. A service another `Watches` already serves is refused.
    pub(super) fn ensure_owner(self: &Arc<Hub>, service: &Arc<Service>, chat: &Arc<Chat>, rows: &Workspaces) -> Result<(), ChatCode> {
        let workspace = service.workspace().to_string();
        let mut desks = lock(&self.desks);
        let desk = desks.entry(workspace.clone()).or_default();
        if desk.running {
            return Ok(());
        }
        let Some(watches) = service.watches() else {
            // An ended service gives none, and no owner would ever see its end: turn its workspace off here.
            drop(desks);
            chat.service_ended(service);
            return Err(ChatCode::Unavailable);
        };
        desk.running = true;
        let (ask, ending) = mpsc::unbounded_channel();
        desk.ending = Some(ask);
        let kick = desk.kick.clone();
        drop(desks);
        let own = Own { hub: self.clone(), workspace, kick, service: service.clone(), chat: chat.clone(), rows: rows.clone(), known: BTreeSet::new(), told: BTreeSet::new(), mail: None };
        tokio::spawn(own.run(watches, ending));
        Ok(())
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
    /// Watch `target`'s channel of its workspace. The daemon holds at most `MAX_CHANNELS` (workspace, channel) pairs in
    /// all, every workspace's counted together (the model's bound, which wire's `MAX_WATCHED` repeats, so the watched
    /// records' memory is today's bound whatever the number of workspaces), so a connection, whose slots are among them,
    /// holds at most that many too; a connection may join a channel another holds, and a watch it already holds is
    /// replaced by this one. One channel id in two workspaces is two slots.
    pub(crate) fn claim(&self, target: ChatTarget) -> Result<(), ChatCode> {
        let mut table = lock(&self.hub.table);
        let key = (target.workspace.clone(), target.channel.as_str().to_string());
        let held = table.pairs();
        if held.len() >= MAX_CHANNELS && !held.contains(&key) {
            return Err(ChatCode::TooManyWatches);
        }
        table.slots.retain(|(c, t)| !(*c == self.id && pair(t) == (key.0.as_str(), key.1.as_str())));
        table.slots.push((self.id, target));
        Ok(())
    }

    /// End this connection's watch of the channel of `workspace`, and no other's.
    pub(crate) fn release(&self, workspace: &str, channel: &str) {
        lock(&self.hub.table).slots.retain(|(c, t)| !(*c == self.id && pair(t) == (workspace, channel)));
        self.hub.kick(workspace);
    }

    pub(crate) fn is(&self, id: u64) -> bool {
        self.id == id
    }

    pub(crate) fn watches(&self, workspace: &str, channel: &str) -> bool {
        lock(&self.hub.table).holds(self.id, workspace, channel)
    }

    /// The state to answer a watch with: complete when the workspace's owner holds the channel's snapshot, else a
    /// snapshot is under way or about to begin. Never `needed`: a channel that owes one is told by `chat.status`.
    pub(crate) fn state(&self, workspace: &str, channel: &str) -> ChatWatchState {
        match lock(&self.hub.table).states.get(&(workspace.to_string(), channel.to_string())) {
            Some(ChatWatchState::Complete) => ChatWatchState::Complete,
            _ => ChatWatchState::Pending,
        }
    }

    /// Register the watch, start its workspace's owner if it is not running and have it begin the snapshot. A channel whose
    /// last snapshot Slack refused because the app is not in it is refused `not_in_channel`, its slot kept, so the window
    /// says what to do; the snapshot this watch began tries again, and the first one that succeeds ends the refusal.
    pub(crate) async fn watch(&self, target: ChatTarget, service: &Arc<Service>, chat: &Arc<Chat>, rows: &Workspaces) -> Result<ChatWatchState, ChatCode> {
        let (workspace, channel) = (target.workspace.clone(), target.channel.as_str().to_string());
        self.claim(target)?;
        if let Err(code) = self.hub.ensure_owner(service, chat, rows) {
            self.release(&workspace, &channel);
            return Err(code);
        }
        self.hub.kick(&workspace);
        // A test's hold point, keyed by the hub's address, between the registration and the reply. The owner is kicked
        // above, so its snapshot runs while this is held; `chat.watch.snapshot` holds that.
        sot_platform::hold_point_async!("chat.watch.between", Arc::as_ptr(&self.hub) as usize);
        if let Some(code) = lock(&self.hub.table).refused.get(&(workspace.clone(), channel.clone())).copied() {
            return Err(code);
        }
        Ok(self.state(&workspace, &channel))
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
        self.conn.hub.kick_all();
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
    /// The workspace this owner serves: it syncs, publishes, tells and ends only that workspace's slots and states.
    workspace: String,
    /// Its desk's kick.
    kick: Arc<Notify>,
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
    /// The state folder, on this computer's disk.
    dir: std::path::PathBuf,
    /// Channel and its mapped folder.
    folders: Vec<(String, String)>,
    tx: mpsc::Sender<Done>,
    wake: Option<tokio::time::Instant>,
    /// The start's read, the filing in flight (with its place at the daemon's filing slot) and the pass in flight.
    read: JoinHandle<()>,
    filing: Option<(JoinHandle<()>, Arc<Mutex<Slot>>)>,
    pass: Option<JoinHandle<()>>,
}

/// Where a filing job stands at the daemon's filing slot: a job still waiting has filed nothing, so a delivery's end
/// drops it at once; one that holds the slot is waited for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Waiting,
    Holding,
    Dropped,
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

/// One filing: the author named as a read names it (a Slack call of this workspace's, so before the slot), then at the
/// daemon's one filing slot: the slot taken, `FILE_GAP` after the daemon's last filing returned, the guarded append, and
/// the slot freed with the return recorded. A job its delivery's end dropped before it held the slot files nothing.
async fn file_job(env: Env, rows: Workspaces, service: Arc<Service>, folder: String, mut f: Filing, filing: Arc<FilingSlot>, at: Arc<Mutex<Slot>>) -> Done {
    #[cfg(test)]
    assert!(!PANIC_ON.lock().unwrap().contains(&f.record.ts.as_str().to_string()), "a test's filing job fails here");
    let (channel, ts, live) = (f.channel.clone(), f.record.ts.clone(), f.live);
    if let Ok(api) = service.api() {
        api.fill_names(std::slice::from_mut(&mut f.record)).await;
    }
    let filed = filing
        .file(&at, async {
            sot_platform::hold_point_async!("chat.mail.filing", folder);
            mail::file(&env, &rows, &folder, &f.record).await
        })
        .await;
    Done::Filed { result: filed.unwrap_or_else(|| Err("ended".into())), channel, ts, live }
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
    api.history(&query, None).await.map(|p| p.records).map_err(|e| read_code(&e))
}

impl Own {
    async fn run(mut self, mut watches: Watches, mut ending: mpsc::UnboundedReceiver<oneshot::Sender<()>>) {
        let (done_tx, mut done_rx) = mpsc::channel::<Fetched>(MAX_CHANNELS);
        let (mail_tx, mut mail_rx) = mpsc::channel::<Done>(8);
        self.mail = self.open_mail(&mut watches, mail_tx);
        loop {
            let at = self.mail.as_ref().and_then(|m| m.wake);
            tokio::select! {
                _ = self.kick.notified() => self.sync(&mut watches, &done_tx),
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
        // A test's hold point, keyed by the hub's address, between the service's stop and the owner's end.
        sot_platform::hold_point_async!("chat.owner.end", Arc::as_ptr(&self.hub) as usize);
        // A service that ended on its own turns the chat off before the owner is idle (a reload waits for the owner's
        // end, so it never meets the new service with this call), and the chat is off before the watchers are told.
        // A stop took the delivery already; an end did not, so its filing in flight returns here, after the phase left
        // `Running` and before the chat is off. The stop's request for the delivery is answered by the receiver's drop.
        self.chat.service_ended(&self.service);
        self.end_delivery().await;
        drop(ending);
        if self.service.ended().is_some() {
            let _ = tokio::time::timeout(2 * super::STOP_WITHIN, self.chat.stopped_of(&self.workspace)).await;
        }
        self.end();
    }

    fn hand_over(&mut self, arrived: Arrived) {
        if let Some(m) = &mut self.mail {
            m.delivery.hand_over(arrived);
        }
    }

    /// The delivery for its workspace's delivering channels, whose states are read and whose records the model hands
    /// over from here on; the start's read of the inboxes begins. A channel whose state cannot be read stays off.
    fn open_mail(&self, watches: &mut Watches, tx: mpsc::Sender<Done>) -> Option<Mail> {
        // The plan of the unit that runs this owner's service: a reload that keeps the unit changes nothing it reads, and a
        // service a reload stopped or replaced reads none.
        let plan = self.chat.plan_of(&self.workspace, &self.service)?;
        let dir = sot_platform::host::state_dir::daemon_state_dir_in(&self.chat.env).join("chat");
        let (mut states, mut folders) = (Vec::new(), Vec::new());
        for (folder, channel, floor) in plan.mappings() {
            let Some(floor) = floor.cloned() else { continue };
            match sot_chat::state::State::load(&dir, &self.workspace, channel, &floor) {
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
        Some(Mail { delivery: Delivery::new(timing, std::time::Instant::now(), states), dir, folders, tx, wake: None, read, filing: None, pass: None })
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
        self.end_delivery().await;
        drop(done.send(()));
    }

    /// The delivery out of use, its filing in flight returned (bounded by `MAIL_END`); a filing still waiting for the
    /// daemon's slot has filed nothing and is dropped at once, so another workspace's filings never spend this wait.
    async fn end_delivery(&mut self) {
        if let Some((filing, at)) = self.mail.take().and_then(|mut m| m.filing.take()) {
            let waiting = {
                let mut at = lock(&at);
                let waiting = *at == Slot::Waiting;
                if waiting {
                    *at = Slot::Dropped;
                }
                waiting
            };
            if waiting {
                filing.abort();
            } else {
                let _ = tokio::time::timeout(MAIL_END, filing).await;
            }
        }
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
                    let at = Arc::new(Mutex::new(Slot::Waiting));
                    let job = file_job(env, rows, service, folder, f, self.hub.filing.clone(), at.clone());
                    m.filing = Some((run_job(tx, job, failed), at));
                }
                Action::Pass { order, pace } => {
                    let channel = order.state.channel().to_string();
                    m.pass = Some(run_job(tx, pass_job(env, rows, service, folder(&channel), order, pace), Done::Skipped(channel, "job_failed")));
                }
                Action::Commit { channel, boundary } => {
                    let (dir, workspace) = (m.dir.clone(), self.workspace.clone());
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
            // A channel no connection watches in this workspace (a delivering one) tells no one.
            if !lock(&self.hub.table).slots.iter().any(|(_, t)| pair(t) == (self.workspace.as_str(), channel.as_str())) {
                continue;
            }
            let workspace = self.workspace.clone();
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
        let want = lock(&self.hub.table).channels_of(&self.workspace);
        for gone in self.known.difference(&want).cloned().collect::<Vec<_>>() {
            watches.unwatch(&gone);
            lock(&self.hub.table).refused.remove(&(self.workspace.clone(), gone.clone()));
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
            let targets: Vec<(u64, ChatTarget)> = lock(&self.hub.table).slots.iter().filter(|(_, t)| pair(t) == (self.workspace.as_str(), channel.as_str())).cloned().collect();
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
            lock(&self.hub.table).slots.retain(|(c, t)| !(c == conn && pair(t) == (self.workspace.as_str(), f.channel.as_str())));
        }
        let gap = self.service.gap().is_some();
        let released = f.stale.iter().map(|c| (*c, ChatStatusEvt { service: ChatService::Running, gap, resync: ChatChannel::try_from(f.channel.clone()).into_iter().collect() })).collect();
        if !f.stale.is_empty() {
            self.hub.send(Evt::Status(Arc::new(released)));
        }
        // A refusal a person clears stands for the channel's next watch until a snapshot succeeds.
        let key = (self.workspace.clone(), f.channel.clone());
        match &f.result {
            Ok(_) => {
                lock(&self.hub.table).refused.remove(&key);
            }
            Err(ChatCode::NotInChannel) => {
                lock(&self.hub.table).refused.insert(key, ChatCode::NotInChannel);
            }
            Err(_) => {}
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
        let workspace = self.workspace.clone();
        table.states.retain(|(w, _), _| *w != workspace);
        for channel in &self.known {
            let state = match watches.state(channel) {
                Some(State::Complete) => ChatWatchState::Complete,
                Some(State::Pending) => ChatWatchState::Pending,
                _ => {
                    needed.insert(channel.clone());
                    ChatWatchState::Needed
                }
            };
            table.states.insert((workspace.clone(), channel.clone()), state);
        }
        // A service that is stopping makes every channel read as owing a snapshot: `end` says so once, as the stop. The
        // status is read after the states: a stop sets its phase before it ends the claim they read.
        if needed == self.told || !matches!(self.service.status(), Status::Running { .. }) {
            return;
        }
        self.told = needed.clone();
        if !needed.is_empty() {
            let status = table.status(&workspace, ChatService::Running, self.service.gap().is_some(), &needed);
            drop(table);
            self.hub.send(Evt::Status(Arc::new(status)));
        }
    }

    /// The service stopped: every connection watching in its workspace is told its channels there need a snapshot, every
    /// watch of the workspace ends (another workspace's stay), and the desk is idle again, for the next service a reload
    /// starts (a stopped service gives no second `Watches`).
    fn end(self) {
        let phase = match self.service.status() {
            Status::Stopping => ChatService::Stopping,
            _ => ChatService::Stopped,
        };
        let status = lock(&self.hub.table).end_of(&self.workspace, phase);
        self.hub.send(Evt::Status(Arc::new(status)));
        if let Some(desk) = lock(&self.hub.desks).get_mut(&self.workspace) {
            (desk.running, desk.ending) = (false, None);
        }
        self.hub.idle.notify_waiters();
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
