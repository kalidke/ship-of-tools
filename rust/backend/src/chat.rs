//! The project chat in the daemon: the one place `sotd` names `sot-chat`. It decides whether this daemon is the
//! configured hub before it reads any chat file, builds the row facts chat's mappings are admitted against, starts the
//! one service off the boot path, and retires it through lifecycle's registry. `ops` serves the request ops.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use sot_chat::config::{self, Config, ConfigError, Credentials, Folder, Reader, Resolution};
use sot_chat::delivery::Timing;
use sot_chat::http::Http;
use sot_chat::service::Service;
use sot_platform::host::env::Env;
use sot_protocol::ops::{ChatCode, ChatTarget};
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

pub(crate) mod mail;
pub(crate) mod ops;
pub(crate) mod watch;

use crate::lifecycle::retire::{Participant, Registry, RETIRE_WAIT};
use crate::rows::{Workspace, Workspaces};

/// How long the service's own stop may take before it aborts the listener: inside lifecycle's bound, so a stop with no
/// delivery to end is reported complete. A filing in flight is waited for first (`Hub::end_mail`, up to 15 s), and a
/// retirement that outlasts its bound says so.
pub(crate) const STOP_WITHIN: Duration = Duration::from_millis(500);
const _: () = assert!(STOP_WITHIN.as_millis() < RETIRE_WAIT.as_millis());

/// What the composition needs of a running service. `Service` is the product; the test's listener is the other.
pub(crate) trait Listener: Send + Sync + 'static {
    fn stop(&self, within: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

impl Listener for Service {
    fn stop(&self, within: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(Service::stop(self, within))
    }
}

/// How long a start refused `held` waits to try again, and how often a running chat asks whether another computer
/// became the hub.
pub(crate) const HUB_CHECK: Duration = Duration::from_secs(60);

/// How a start is made: from the admitted configuration and credentials and the lock folder, once per try (a start
/// refused `held` is tried again). The product's is [`start_service`].
pub(crate) type Starter<L> = Box<
    dyn for<'a> FnMut(&'a Config, Credentials, &'a Path) -> Pin<Box<dyn Future<Output = Result<Arc<L>, &'static str>> + Send + 'a>>
        + Send,
>;

pub(crate) fn start_service<'a>(config: &'a Config, credentials: Credentials, dir: &'a Path) -> Pin<Box<dyn Future<Output = Result<Arc<Service>, &'static str>> + Send + 'a>> {
    let dir = dir.to_path_buf();
    Box::pin(async move {
        let http = Http::new().map_err(|e| e.code())?;
        Service::start(config, credentials, http, &dir).await.map(Arc::new).map_err(|e| e.code())
    })
}

/// Why the chat is not running here, or that it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Not the configured hub: no chat file was opened.
    NotHub,
    /// `chat.toml` is missing or refused (the hub's, so its mappings were admitted against this daemon's rows).
    Refused(ConfigError),
    /// `chat-secrets.toml` is missing or refused; read only after the hub and `chat.toml` were admitted.
    Secrets(ConfigError),
    /// The service did not start: a fixed code.
    Failed(&'static str),
    Starting,
    Running,
    Stopping,
    Stopped,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::NotHub => f.write_str(Resolution::NotHub.explain()),
            Status::Refused(e @ ConfigError::Missing) => write!(f, "{e}"),
            Status::Refused(e) => write!(f, "{}: {e}", config::CONFIG_FILE),
            Status::Secrets(ConfigError::Missing) => write!(f, "{} is missing from the config folder", config::SECRETS_FILE),
            Status::Secrets(e) => write!(f, "{}: {e}", config::SECRETS_FILE),
            Status::Failed(code) => write!(f, "the chat service did not start: {code}"),
            Status::Starting => f.write_str("the chat service is starting"),
            Status::Running => f.write_str("the chat service is running"),
            Status::Stopping => f.write_str("the chat service is stopping"),
            Status::Stopped => f.write_str("the chat service is stopped"),
        }
    }
}

enum Phase<L> {
    Off(Status),
    Starting(AbortHandle),
    Running(Arc<L>),
    Stopping,
}

struct Shared<L> {
    phase: Mutex<Phase<L>>,
    off: Condvar,
    /// Woken when a start ends, in `Running` or in `Off`.
    started: Notify,
}

impl<L> Shared<L> {
    fn new() -> Arc<Self> {
        Arc::new(Shared { phase: Mutex::new(Phase::Off(Status::NotHub)), off: Condvar::new(), started: Notify::new() })
    }

    fn lock(&self) -> MutexGuard<'_, Phase<L>> {
        self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_off(&self, status: Status) {
        *self.lock() = Phase::Off(status);
        self.off.notify_all();
        self.started.notify_waiters();
    }
}

/// A running chat's watch on the hub fact: each `check`, while the chat runs, it asks the topology and, when another
/// computer is named the hub, stops the service (freeing the lock) and leaves the status `NotHub`. It holds the chat
/// only between asks, so a dropped chat ends it.
async fn step_down_when_another_hub<L: Listener>(chat: std::sync::Weak<Chat<L>>, env: Env, check: Duration) {
    loop {
        tokio::time::sleep(check).await;
        let Some(chat) = chat.upgrade() else { return };
        if !matches!(*chat.shared.lock(), Phase::Running(_)) {
            return;
        }
        let e = env.clone();
        if tokio::task::spawn_blocking(move || names_another_hub(&e)).await.unwrap_or(false) {
            chat.stop_as(Status::NotHub);
            return;
        }
    }
}

/// One row's folder as this daemon knows it: its canonical spelling and whether it is the inert anchor.
pub(crate) struct RowFact {
    pub(crate) canonical: PathBuf,
    pub(crate) inert: bool,
}

/// The one canonical spelling of a folder: absolute, resolved through symlinks, `.` and `..` and a trailing slash, and
/// on Windows in the system's own letter case and separators without the verbatim prefix. `None` for a relative
/// spelling (it would mean this daemon's working folder) and for a folder that does not exist.
fn canonical(folder: &Path) -> Option<PathBuf> {
    if !folder.is_absolute() {
        return None;
    }
    std::fs::canonicalize(folder).ok().map(sot_platform::host::paths::simplify_verbatim)
}

fn fact_of(workspaces: &Workspaces, ws: &Workspace) -> Option<RowFact> {
    canonical(&ws.project_root).map(|c| RowFact { canonical: c, inert: workspaces.is_inert_default_anchor(ws) })
}

/// The facts of this daemon's rows, as `Config::load` admits mappings against them.
pub(crate) fn row_facts(workspaces: &Workspaces) -> Vec<RowFact> {
    workspaces.list().iter().filter_map(|ws| fact_of(workspaces, ws)).collect()
}

/// The registry's current entry for the canonical row id, read now. The id is matched exactly (the registry's
/// `resolve` also takes a slug, which the filter rejects), and no lock is held past the read.
fn current_row(workspaces: &Workspaces, id: &str) -> Option<RowFact> {
    let ws = workspaces.resolve(Some(id)).filter(|ws| ws.workspace_id == id)?;
    fact_of(workspaces, &ws)
}

/// A mapping's folder as chat's `Rows` asks for it: through the same canonicalizer as the rows' own roots.
fn folder_of(rows: &[RowFact], folder: &str) -> Option<Folder> {
    let canon = canonical(Path::new(folder))?;
    let mut same = rows.iter().filter(|r| r.canonical == canon).peekable();
    same.peek()?;
    Some(Folder { canonical: canon.to_string_lossy().into_owned(), inert: same.all(|r| r.inert) })
}

/// Strict hub admission: topology's current load and parse, which must name this computer as its one explicit hub. A
/// missing or invalid topology, and a computer with no name, are not the hub (`comm_topology_hub` accepts a missing
/// file, and the store keeps an earlier parse after the file is gone; neither may admit the chat).
pub(crate) fn is_hub(env: &Env) -> bool {
    let me = crate::comm::mail::filer::comm_self_host_in(env);
    matches!(sot_topology::load_in(env), Ok(Some((_, t))) if !me.is_empty() && t.hub == me)
}

/// Whether `topology` names a computer other than `me` as the hub: only a topology that parses, and a self-name that is
/// not empty. A missing or unparsable topology and a computer with no name name no one else.
fn names_other_hub(topology: &Result<Option<(PathBuf, sot_topology::Topology)>, String>, me: &str) -> bool {
    matches!(topology, Ok(Some((_, t))) if !me.is_empty() && t.hub != me)
}

/// [`names_other_hub`] for topology's current load: the one answer a running chat steps down on, and each filing asks
/// first (D25's window).
pub(crate) fn names_another_hub(env: &Env) -> bool {
    names_other_hub(&sot_topology::load_in(env), &crate::comm::mail::filer::comm_self_host_in(env))
}

/// The daemon's chat. Decided at construction: a daemon that is not the hub holds no configuration and no service,
/// and never reads a chat file again. Once it runs, it asks the topology each [`HUB_CHECK`] and steps down when
/// another computer is named the hub.
pub(crate) struct Chat<L: Listener = Service> {
    /// `None` on a daemon that is not the hub.
    config: Option<Result<Config, ConfigError>>,
    shared: Arc<Shared<L>>,
    /// The runtime that stops the service; `None` only for a chat that never started one.
    handle: Option<Handle>,
    /// Which connection watches which channel, and the owner task that serves them.
    hub: Arc<watch::Hub>,
    /// The environment `begin` was given: the owner's state folder and every filing are read through it.
    env: Env,
}

impl<L: Listener> Chat<L> {
    /// Admit, then start. In order: the hub fact; `chat.toml` against `rows`; `chat-secrets.toml` (the only open of it
    /// in the daemon, through `read`); the lock folder (the comm folder's `chat/`); registration with `registry`; then `start` on a task of its
    /// own, so boot never waits on Slack. A missing `chat.toml` leaves the chat off and writes nothing. Rows made later
    /// are not seen: a mapping to a folder no row had at this call is refused as `UnknownFolder` until the daemon
    /// restarts. `check` is the pace of the retry of a start refused `held` and of the hub fact's asks (`HUB_CHECK`; the
    /// tests shorten it). Must run inside the daemon's runtime.
    fn begin(env: &Env, rows: &[RowFact], read: Reader, registry: &Registry, start: Starter<L>, check: Duration) -> Arc<Chat<L>> {
        let shared = Shared::new();
        let chat = |config, status| {
            *shared.lock() = Phase::Off(status);
            Arc::new(Chat { config, shared: shared.clone(), handle: Some(Handle::current()), hub: watch::Hub::new(), env: env.clone() })
        };
        if !is_hub(env) {
            return chat(None, Status::NotHub);
        }
        let Some(dir) = config::config_dir(env) else {
            return chat(Some(Err(ConfigError::Missing)), Status::Refused(ConfigError::Missing));
        };
        let config = Config::load(&dir, read, &|f| folder_of(rows, f));
        let credentials = match &config {
            Err(e) => {
                let refused = Status::Refused(*e);
                return chat(Some(config), refused);
            }
            Ok(admitted) => match Credentials::read(&dir, read, admitted) {
                Ok(c) => c,
                Err(e) => return chat(Some(config), Status::Secrets(e)),
            },
        };
        // The comm folder's, not this daemon's: every daemon that files into these inboxes takes the same lock.
        let Some(locks) = crate::comm::home::chat_dir(env) else { return chat(Some(config), Status::Failed("lock")) };
        let this = chat(Some(config), Status::Stopped);
        // The phase lock is taken before the registration and held until `Starting` is in place: a retirement that
        // reaches `request_stop` in between waits for it, and so finds the start and aborts it. The start task's own
        // completion takes the same lock.
        let (task_chat, shared, env) = (this.clone(), this.shared.clone(), env.clone());
        let mut phase = shared.lock();
        if !registry.register(this.clone()) {
            return this;
        }
        let task = Handle::current().spawn(async move {
            let Some(Ok(config)) = &task_chat.config else { return };
            let mut start = start;
            let mut held = false;
            // A start refused `held` is tried again each `check` until a readable topology names another computer the
            // hub (a missing or unparsable one, as an edit in progress leaves, ends nothing), the phase staying
            // `Starting` so `running` keeps waiting. The credentials are cloned per try and never formatted.
            let made = loop {
                let tokens = Credentials { bot: credentials.bot.clone(), app: credentials.app.clone() };
                match start(config, tokens, &locks).await {
                    Err("held") => {
                        if !std::mem::replace(&mut held, true) {
                            tracing::warn!("project chat is held by another daemon on this comm folder; trying again");
                        }
                        tokio::time::sleep(check).await;
                        let e = env.clone();
                        if tokio::task::spawn_blocking(move || names_another_hub(&e)).await.unwrap_or(false) {
                            break Err(Status::NotHub);
                        }
                    }
                    other => break other.map_err(Status::Failed),
                }
            };
            // No more tries: what the starter and the tokens hold goes now, not with the chat.
            drop((start, credentials));
            let running = {
                let shared = &task_chat.shared;
                let mut phase = shared.lock();
                match (made, matches!(*phase, Phase::Starting(_))) {
                    (Ok(l), true) => {
                        *phase = Phase::Running(l);
                        shared.started.notify_waiters();
                        true
                    }
                    // Retired while the start was under way: the service it made ends at once.
                    (Ok(l), false) => {
                        tokio::spawn(async move { l.stop(STOP_WITHIN).await });
                        false
                    }
                    (Err(status), true) => {
                        drop(phase);
                        match status {
                            Status::Failed(code) => tracing::warn!(code, "project chat did not start"),
                            _ => tracing::warn!("project chat is off: the topology names another computer as the hub"),
                        }
                        shared.set_off(status);
                        false
                    }
                    (Err(_), false) => false,
                }
            };
            if running {
                let weak = Arc::downgrade(&task_chat);
                drop(task_chat);
                step_down_when_another_hub(weak, env, check).await;
            }
        });
        *phase = Phase::Starting(task.abort_handle());
        drop(phase);
        this
    }

    /// `begin` alone, for a test that wants no boot task: the owner then starts with a first watch. `server::run`
    /// builds its chat only through [`Chat::boot`].
    #[cfg(test)]
    pub(crate) fn begin_unowned(env: &Env, rows: &[RowFact], read: Reader, registry: &Registry, start: Starter<L>) -> Arc<Chat<L>> {
        Chat::begin(env, rows, read, registry, start, HUB_CHECK)
    }

    /// A chat that is not the hub, for a test's services.
    #[cfg(test)]
    pub(crate) fn off() -> Arc<Chat<L>> {
        let shared = Shared::new();
        Arc::new(Chat { config: None, shared, handle: Handle::try_current().ok(), hub: watch::Hub::new(), env: Env::empty() })
    }

    pub(crate) fn hub(&self) -> &Arc<watch::Hub> {
        &self.hub
    }

    pub(crate) fn status(&self) -> Status {
        match &*self.shared.lock() {
            Phase::Off(s) => *s,
            Phase::Starting(_) => Status::Starting,
            Phase::Running(_) => Status::Running,
            Phase::Stopping => Status::Stopping,
        }
    }

    /// The running service. Private: a chat action reaches it only through [`Chat::admit`].
    fn service(&self) -> Option<Arc<L>> {
        match &*self.shared.lock() {
            Phase::Running(l) => Some(l.clone()),
            _ => None,
        }
    }

    /// The running service once the start has ended: `None` when it ended any other way (never the hub, refused,
    /// failed, retired).
    async fn running(&self) -> Option<Arc<L>> {
        loop {
            let changed = self.shared.started.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match &*self.shared.lock() {
                Phase::Starting(_) => {}
                Phase::Running(l) => return Some(l.clone()),
                _ => return None,
            }
            changed.await;
        }
    }

    /// M2 for a canonical folder: a daemon that is not the hub says so, and a hub answers from its load. It says
    /// nothing of the service.
    fn mapping(&self, folder: &str) -> Resolution {
        match &self.config {
            None => Resolution::NotHub,
            Some(c) => config::resolve(c, folder),
        }
    }

    /// M3 for `chat.resolve`: the mapping of the row's current folder, and whether mail is delivered for it, which
    /// takes a running service. Not the hub: `NotHub`. A row that is gone: `StaleRow`. The inert anchor: `Unmapped`.
    /// Every service status shows the file's mapping, and only `Running` delivers; a refused `chat.toml` is `Invalid`.
    pub(crate) fn resolve(&self, rows: &Workspaces, row: &str) -> Result<(Resolution, bool), ChatCode> {
        if self.config.is_none() {
            return Ok((Resolution::NotHub, false));
        }
        let fact = current_row(rows, row).ok_or(ChatCode::StaleRow)?;
        if fact.inert {
            return Ok((Resolution::Unmapped, false));
        }
        let resolution = self.mapping(&fact.canonical.to_string_lossy());
        let delivers = resolution.delivers() && self.status() == Status::Running;
        Ok((resolution, delivers))
    }

    /// M3: the one door every chat action that reaches Slack or a file passes, at the moment it is admitted. The
    /// service is running; the row is still the registry's current entry for that id (a removed row, a replaced one under a new id and an id that
    /// never existed are `StaleRow`); it is not the inert anchor (`Unmapped`, as a folder the file does not map); and
    /// its current folder maps, in the configuration loaded at start, to exactly the expected workspace and channel
    /// (anything else is `StaleMapping`). Only then is the service handed out. Limit: the check is at admission, no
    /// rows lock is held across a Slack call, and a call already made is not recalled when its row is removed
    /// afterwards; a later action, a watch's next snapshot included, is admitted again.
    pub(crate) fn admit(&self, rows: &Workspaces, target: &ChatTarget) -> Result<Arc<L>, ChatCode> {
        let service = self.service().ok_or(ChatCode::Unavailable)?;
        let fact = current_row(rows, &target.row).ok_or(ChatCode::StaleRow)?;
        if fact.inert {
            return Err(ChatCode::Unmapped);
        }
        match self.mapping(&fact.canonical.to_string_lossy()) {
            Resolution::Mapped { workspace, channel, .. } => {
                if workspace != target.workspace || channel != target.channel.as_str() {
                    return Err(ChatCode::StaleMapping);
                }
            }
            _ => return Err(ChatCode::Unmapped),
        }
        Ok(service)
    }

    /// The door for an unwatch, which hands out no service: it ends access and calls nothing, so it needs no current
    /// row, and refusing it for a stale one would leave the watch holding one of the bounded slots.
    pub(crate) fn admit_unwatch(&self) -> Result<(), ChatCode> {
        self.service().map(|_| ()).ok_or(ChatCode::Unavailable)
    }

    /// End the start or the service, the chat then `Off(ended)`: `Stopped` for a retirement, `NotHub` for a step-down.
    fn stop_as(&self, ended: Status) {
        let mut phase = self.shared.lock();
        match std::mem::replace(&mut *phase, Phase::Stopping) {
            Phase::Starting(task) => {
                task.abort();
                drop(phase);
                self.shared.set_off(ended);
            }
            Phase::Running(l) => {
                drop(phase);
                let (shared, hub) = (self.shared.clone(), self.hub.clone());
                let stop = async move {
                    // The delivery ends first, its filing in flight returned, while the lock is still held.
                    hub.end_mail().await;
                    l.stop(STOP_WITHIN).await;
                    shared.set_off(ended);
                };
                match &self.handle {
                    Some(h) => drop(h.spawn(stop)),
                    None => unreachable!("a running service was started on a runtime"),
                }
            }
            other => *phase = other,
        }
    }
}

impl Chat<Service> {
    /// The boot's one task: when the service is running, log it once and start the hub's owner, which serves the
    /// socket with no window open. `ensure_owner` stays the one start; a watch that comes first starts it too.
    pub(crate) fn start_owner(self: &Arc<Self>, rows: Workspaces) -> tokio::task::JoinHandle<()> {
        let chat = self.clone();
        tokio::spawn(async move {
            let Some(service) = chat.running().await else { return };
            tracing::info!(status = %Status::Running, "project chat");
            // The mapped channels' names, looked up once in the background (`Api::channel_names`, which holds the
            // lookup to a count and a deadline): `chat.resolve` makes no Slack call and carries what the cache holds, so a
            // window's title can name the channel. A channel that got no name keeps its id, and its code is logged.
            if let (Some(Ok(config)), Ok(api)) = (&chat.config, service.api()) {
                let channels: Vec<String> = config.mappings().map(|(_, channel)| channel.to_string()).collect();
                drop(tokio::spawn(async move {
                    for (channel, code) in api.channel_names(&channels).await {
                        tracing::warn!(code, channel, "project chat: a channel's name was not found");
                    }
                }));
            }
            if let Err(code) = chat.hub.ensure_owner(&service, &chat, &rows) {
                tracing::warn!(?code, "project chat owner did not start");
            }
        })
    }

    /// The daemon's chat as the boot makes it, and as the tests' rig makes it: begin, the boot's log line, and the
    /// owner's task, started while the chat is still `Starting`. `check` is [`Chat::begin`]'s; `timing` is the delivery's (`Timing::PRODUCT`; a test shortens it). Must run inside the daemon's
    /// runtime.
    pub(crate) fn boot(env: &Env, rows: &Workspaces, read: Reader, registry: &Registry, start: Starter<Service>, check: Duration, timing: Timing) -> Arc<Chat> {
        let chat = Chat::begin(env, &row_facts(rows), read, registry, start, check);
        chat.hub.set_timing(timing);
        tracing::info!(status = %chat.status(), "project chat");
        drop(chat.start_owner(rows.clone()));
        chat
    }
}

impl<L: Listener> Participant for Chat<L> {
    fn name(&self) -> &'static str {
        "chat"
    }

    /// The chat has no admission to close yet: a handler's `admit` refuses `unavailable` once the stop begins.
    fn close_admission(&self) {}

    fn request_stop(&self) {
        self.stop_as(Status::Stopped);
    }

    fn wait_retired(&self, within: Duration) -> bool {
        let phase = self.shared.lock();
        let (phase, _) = self
            .shared
            .off
            .wait_timeout_while(phase, within, |p| !matches!(p, Phase::Off(_)))
            .unwrap_or_else(|e| e.into_inner());
        matches!(*phase, Phase::Off(_))
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
pub(crate) mod tests;
