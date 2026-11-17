//! The project chat in the daemon: the one place `sotd` names `sot-chat`. It decides whether this daemon is the
//! configured hub before it reads any chat file, admits chat's mappings against the disk, starts the
//! one service off the boot path, reloads both files and starts again on a change, and retires it through lifecycle's
//! registry. `ops` serves the request ops.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

use sot_chat::config::{self, Config, ConfigError, Credentials, Reader, Resolution};
use sot_chat::delivery::Timing;
use sot_chat::http::Http;
use sot_chat::service::{Service, APP_TOKEN_REFUSED, BOT_TOKEN_REFUSED, WRONG_APP};
use sot_platform::host::env::Env;
use sot_protocol::ops::{ChatCode, ChatTarget};
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

pub(crate) mod mail;
pub(crate) mod ops;
pub(crate) mod setup;
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
/// refused `held` is tried again, and a reload starts again). The product's is [`start_service`].
pub(crate) type Starter<L> = Box<
    dyn for<'a> Fn(&'a Config, Credentials, &'a Path) -> Pin<Box<dyn Future<Output = Result<Arc<L>, &'static str>> + Send + 'a>>
        + Send
        + Sync,
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
    /// The service did not start, or ended on its own: a fixed code (`Service::ended`'s are listed in `setup::unconfigured`).
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
            Status::Failed(code @ (WRONG_APP | APP_TOKEN_REFUSED | BOT_TOKEN_REFUSED)) => write!(f, "the chat service ended: {code}"),
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
    /// The start task and its launch's number: a task completes only against its own launch.
    Starting(AbortHandle, u64),
    Running(Arc<L>),
    Stopping,
}

struct Shared<L> {
    phase: Mutex<Phase<L>>,
    off: Condvar,
    /// Woken when a start ends, in `Running` or in `Off`, and when a stop ends.
    started: Notify,
    /// No start may begin: the chat never registered with lifecycle (not the hub at its boot, or its registration
    /// refused) or its retirement began. Set at construction or under the phase lock, and read under it.
    closed: AtomicBool,
    /// The launches so far, counted under the phase lock.
    launches: AtomicU64,
}

impl<L> Shared<L> {
    fn new(off: Status, closed: bool) -> Arc<Self> {
        let phase = Mutex::new(Phase::Off(off));
        Arc::new(Shared { phase, off: Condvar::new(), started: Notify::new(), closed: AtomicBool::new(closed), launches: AtomicU64::new(0) })
    }

    /// Read under the phase lock.
    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// [`Shared::closed`], taking the phase lock to read it.
    fn closed_now(&self) -> bool {
        let _phase = self.lock();
        self.closed()
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

/// A running service's watch on the hub fact: each `check`, while that service runs, it asks the topology and, when
/// another computer is named the hub, stops the service (freeing the lock) and leaves the status `NotHub`. It holds the
/// chat only between asks, so a dropped chat ends it, and it ends when its service is no longer the one running, so a
/// reload's service has the one watch of its own.
async fn step_down_when_another_hub<L: Listener>(chat: Weak<Chat<L>>, service: Weak<L>, env: Env, check: Duration) {
    loop {
        tokio::time::sleep(check).await;
        let Some(chat) = chat.upgrade() else { return };
        if !matches!(&*chat.shared.lock(), Phase::Running(l) if std::ptr::eq(Arc::as_ptr(l), service.as_ptr())) {
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

/// The registry's current entry for the canonical row id, read now. The id is matched exactly (the registry's
/// `resolve` also takes a slug, which the filter rejects), and no lock is held past the read.
fn current_row(workspaces: &Workspaces, id: &str) -> Option<RowFact> {
    let ws = workspaces.resolve(Some(id)).filter(|ws| ws.workspace_id == id)?;
    fact_of(workspaces, &ws)
}

/// A mapping's folder as chat's `Canon` asks for it: through the same canonicalizer as the rows' own roots, with no row
/// needed (a row made later in the folder is admitted at its first action).
pub(crate) fn canon_of(folder: &str) -> Option<String> {
    canonical(Path::new(folder)).map(|c| c.to_string_lossy().into_owned())
}

/// Hub admission: topology's current load and parse. A computer with no `hosts.toml` is its own hub (a lone computer);
/// a file that parses must name this computer as its hub; an invalid topology and a computer with no name are not the
/// hub (the store keeps an earlier parse after the file is gone, so it is not asked).
pub(crate) fn is_hub(env: &Env) -> bool {
    hub_here(&sot_topology::load_in(env), &crate::comm::mail::filer::comm_self_host_in(env))
}

/// Messaging's rule for its comm folder's hub, with a name required even when the file is missing.
fn hub_here(topology: &Result<Option<(PathBuf, sot_topology::Topology)>, String>, me: &str) -> bool {
    !me.is_empty() && crate::comm::mail::filer::comm_topology_hub(topology, me)
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

/// What one load of the chat's files gave.
struct Load {
    /// `None`: this computer is not the hub, and no chat file was opened.
    config: Option<Result<Config, ConfigError>>,
    /// The status the chat is off with until a start: `Stopped` when it may start.
    off: Status,
    /// What a start needs: both files admitted and the lock folder made.
    start: Option<(Credentials, PathBuf)>,
}

/// The daemon's chat. Decided at construction: a daemon that is not the hub at its boot holds no configuration and no
/// service, and never reads a chat file again (a hub that stepped down reads both files again at its next reload). Once it runs, it asks the topology each [`HUB_CHECK`] and steps down when
/// another computer is named the hub.
pub(crate) struct Chat<L: Listener = Service> {
    /// What the last load gave (`None` on a daemon that is not the hub), swapped by a reload only while no service runs,
    /// so a service and its owner see one configuration for their life.
    config: Mutex<Arc<Option<Result<Config, ConfigError>>>>,
    shared: Arc<Shared<L>>,
    /// The runtime that stops the service; `None` only for a chat that never started one.
    handle: Option<Handle>,
    /// Which connection watches which channel, and the owner task that serves them.
    hub: Arc<watch::Hub>,
    /// The environment `begin` was given: the owner's state folder and every filing are read through it.
    env: Env,
    /// How a start is made, the boot's and each reload's.
    start: Arc<Starter<L>>,
    /// How a connect checks its tokens with Slack before anything is saved.
    verify: setup::Verifier,
    /// `begin`'s pace of the retry of a held start and of the hub fact's asks.
    check: Duration,
    /// One reload at a time.
    setup: tokio::sync::Mutex<()>,
}

impl<L: Listener> Chat<L> {
    /// The one load, the boot's and each reload's, and the only caller of `Config::load` and `Credentials::read`. In
    /// order: the hub fact; `chat.toml` against the disk; `chat-secrets.toml` (the only open of it in the daemon, through
    /// `read`); the lock folder (the comm folder's `chat/`). A missing `chat.toml` leaves the chat off and writes
    /// nothing. A mapping needs no row: one whose folder does not exist is ignored and logged by its ordinal, and a row
    /// made later in a mapped folder is admitted at its first action.
    fn load(env: &Env, read: Reader) -> Load {
        let off = |config, off| Load { config, off, start: None };
        if !is_hub(env) {
            return off(None, Status::NotHub);
        }
        let Some(dir) = config::config_dir(env) else {
            return off(Some(Err(ConfigError::Missing)), Status::Refused(ConfigError::Missing));
        };
        let config = Config::load(&dir, read, &canon_of);
        if let Ok(admitted) = &config {
            for n in admitted.ignored() {
                tracing::warn!(mapping = n, "project chat: a mapping's folder does not exist and is ignored");
            }
        }
        let credentials = match &config {
            Err(e) => {
                let refused = Status::Refused(*e);
                return off(Some(config), refused);
            }
            Ok(admitted) => match Credentials::read(&dir, read, admitted) {
                Ok(c) => c,
                Err(e) => return off(Some(config), Status::Secrets(e)),
            },
        };
        if credentials.user.is_none() {
            tracing::warn!("chat-secrets.toml holds no user token: a person's post waits for Connect Slack in the Chat drawer");
        }
        // The comm folder's, not this daemon's: every daemon that files into these inboxes takes the same lock.
        let Some(locks) = crate::comm::home::chat_dir(env) else { return off(Some(config), Status::Failed("lock")) };
        Load { config: Some(config), off: Status::Stopped, start: Some((credentials, locks)) }
    }

    /// Load, register, then start. A hub registers with `registry` whatever its load gave, so a start a reload makes
    /// later retires with the daemon; a daemon that is not the hub registers nothing and never starts. The start runs on
    /// a task of its own, so boot never waits on Slack. `check` is the pace of the retry of a start refused `held` and
    /// of the hub fact's asks (`HUB_CHECK`; the tests shorten it). Must run inside the daemon's runtime.
    fn begin(env: &Env, read: Reader, registry: &Registry, start: Starter<L>, verify: setup::Verifier, check: Duration) -> Arc<Chat<L>> {
        let load = Self::load(env, read);
        let hub = load.config.is_some();
        let shared = Shared::new(load.off, !hub);
        let (config, handle, start) = (Mutex::new(Arc::new(load.config)), Some(Handle::current()), Arc::new(start));
        let this = Arc::new(Chat { config, shared, handle, hub: watch::Hub::new(), env: env.clone(), start, verify, check, setup: Default::default() });
        if !hub {
            return this;
        }
        // The phase lock is taken before the registration and held until `Starting` is in place: a retirement that
        // reaches `request_stop` in between waits for it, and so finds the start and aborts it. The start task's own
        // completion takes the same lock.
        let mut phase = this.shared.lock();
        if !registry.register(this.clone()) {
            this.shared.closed.store(true, Ordering::SeqCst);
        } else if let Some((credentials, locks)) = load.start {
            this.launch(&mut phase, credentials, locks);
        }
        drop(phase);
        this
    }

    /// The configuration of the last load.
    fn config(&self) -> Arc<Option<Result<Config, ConfigError>>> {
        self.config.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The start, on a task, the phase then `Starting`: tried again each `check` while refused `held`, then `Running`
    /// with a hub fact's watch of its own, or off with its status. The caller holds the phase lock, and has found the
    /// chat not closed under it.
    fn launch(self: &Arc<Self>, phase: &mut Phase<L>, credentials: Credentials, locks: PathBuf) {
        let (task_chat, env, check, start) = (self.clone(), self.env.clone(), self.check, self.start.clone());
        let launch = self.shared.launches.fetch_add(1, Ordering::SeqCst);
        let task = Handle::current().spawn(async move {
            let loaded = task_chat.config();
            let Some(Ok(config)) = loaded.as_ref() else { return };
            let mut held = false;
            // A start refused `held` is tried again each `check` until a readable topology names another computer the
            // hub (a missing or unparsable one, as an edit in progress leaves, ends nothing), the phase staying
            // `Starting` so `running` keeps waiting. The credentials are cloned per try and never formatted.
            let made = loop {
                match start(config, credentials.clone(), &locks).await {
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
            // No more tries: the tokens go now, not with the chat.
            drop((start, credentials));
            let running = {
                let shared = &task_chat.shared;
                let mut phase = shared.lock();
                match (made, matches!(*phase, Phase::Starting(_, n) if n == launch)) {
                    (Ok(l), true) => {
                        let mine = Arc::downgrade(&l);
                        *phase = Phase::Running(l);
                        shared.started.notify_waiters();
                        Some(mine)
                    }
                    // Retired while the start was under way: the service it made ends at once.
                    (Ok(l), false) => {
                        tokio::spawn(async move { l.stop(STOP_WITHIN).await });
                        None
                    }
                    (Err(status), true) => {
                        // Off under the lock that found this launch current: a later launch's `Starting` is never
                        // overwritten.
                        *phase = Phase::Off(status);
                        drop(phase);
                        shared.off.notify_all();
                        shared.started.notify_waiters();
                        match status {
                            Status::Failed(code) => tracing::warn!(code, "project chat did not start"),
                            _ => tracing::warn!("project chat is off: the topology names another computer as the hub"),
                        }
                        None
                    }
                    (Err(_), false) => None,
                }
            };
            if let Some(mine) = running {
                let weak = Arc::downgrade(&task_chat);
                drop((task_chat, loaded));
                step_down_when_another_hub(weak, mine, env, check).await;
            }
        });
        *phase = Phase::Starting(task.abort_handle(), launch);
    }

    /// `begin` alone, for a test that wants no boot task: the owner then starts with a first watch. `server::run`
    /// builds its chat only through [`Chat::boot`].
    #[cfg(test)]
    pub(crate) fn begin_unowned(env: &Env, read: Reader, registry: &Registry, start: Starter<L>) -> Arc<Chat<L>> {
        Chat::begin(env, read, registry, start, setup::Verifier::none(), HUB_CHECK)
    }

    /// A chat that is not the hub, for a test's services.
    #[cfg(test)]
    pub(crate) fn off() -> Arc<Chat<L>> {
        let shared = Shared::new(Status::NotHub, true);
        let (config, handle) = (Mutex::new(Arc::new(None)), Handle::try_current().ok());
        let start: Arc<Starter<L>> = Arc::new(Box::new(|_, _, _| Box::pin(std::future::pending())));
        Arc::new(Chat { config, shared, handle, hub: watch::Hub::new(), env: Env::empty(), start, verify: setup::Verifier::none(), check: HUB_CHECK, setup: Default::default() })
    }

    pub(crate) fn hub(&self) -> &Arc<watch::Hub> {
        &self.hub
    }

    pub(crate) fn status(&self) -> Status {
        match &*self.shared.lock() {
            Phase::Off(s) => *s,
            Phase::Starting(..) => Status::Starting,
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
                Phase::Starting(..) => {}
                Phase::Running(l) => return Some(l.clone()),
                _ => return None,
            }
            changed.await;
        }
    }

    /// M2 for a canonical folder: a daemon that is not the hub says so, and a hub answers from its load. It says
    /// nothing of the service.
    fn mapping(&self, folder: &str) -> Resolution {
        match self.config().as_ref() {
            None => Resolution::NotHub,
            Some(c) => config::resolve(c, folder),
        }
    }

    /// M3 for `chat.resolve`: the mapping of the row's current folder, and whether mail is delivered for it, which
    /// takes a running service. Not the hub: `NotHub`. A row that is gone: `StaleRow`. The inert anchor: `Unmapped`.
    /// Every service status shows the file's mapping, and only `Running` delivers; a refused `chat.toml` is `Invalid`.
    pub(crate) fn resolve(&self, rows: &Workspaces, row: &str) -> Result<(Resolution, bool), ChatCode> {
        if self.config().is_none() {
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
    /// its current folder maps, in the configuration of the last load, to exactly the expected workspace and channel
    /// (anything else is `StaleMapping`). Only then is the service handed out. Limit: the check is at admission, no
    /// rows lock is held across a Slack call, and a call already made is not recalled when its row is removed
    /// afterwards; a later action, a watch's next snapshot included, is admitted again.
    pub(crate) fn admit(&self, rows: &Workspaces, target: &ChatTarget) -> Result<Arc<L>, ChatCode> {
        let (service, fact) = self.admit_row(rows, &target.row)?;
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

    /// [`Chat::admit`] without the mapping check, for a pairing: the row may map to nothing yet. The service is running
    /// and the row is the registry's current entry and not the inert anchor; answers the service and the row's folder.
    pub(crate) fn admit_row(&self, rows: &Workspaces, row: &str) -> Result<(Arc<L>, RowFact), ChatCode> {
        let service = self.service().ok_or(ChatCode::Unavailable)?;
        let fact = current_row(rows, row).ok_or(ChatCode::StaleRow)?;
        if fact.inert {
            return Err(ChatCode::Unmapped);
        }
        Ok((service, fact))
    }

    /// The door for an unwatch, which hands out no service: it ends access and calls nothing, so it needs no current
    /// row, and refusing it for a stale one would leave the watch holding one of the bounded slots.
    pub(crate) fn admit_unwatch(&self) -> Result<(), ChatCode> {
        self.service().map(|_| ()).ok_or(ChatCode::Unavailable)
    }

    /// End the start or the service, the chat then `Off(ended)`: `Stopped` for a retirement, `NotHub` for a step-down,
    /// `Failed(code)` for a service that ended on its own. Whether this call began the end: a chat already stopping or off
    /// is left as it is.
    fn stop_as(&self, ended: Status) -> bool {
        let mut phase = self.shared.lock();
        match std::mem::replace(&mut *phase, Phase::Stopping) {
            Phase::Starting(task, _) => {
                task.abort();
                drop(phase);
                self.shared.set_off(ended);
                true
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
                true
            }
            other => {
                *phase = other;
                false
            }
        }
    }
}

/// How long a reload waits for what runs to stop, and then for its start to end.
pub(crate) const SETUP_WAIT: Duration = Duration::from_secs(30);

/// The setup lock, held: one reload, or one edit of the files and the reload after it, at a time. Made only by
/// [`Chat::setup`], and a reload takes it, so a holder edits the files and reloads with no second lock to deadlock on.
pub(crate) struct Setup<'a>(tokio::sync::MutexGuard<'a, ()>);

impl<L: Listener> Chat<L> {
    /// Take the setup lock, waiting for a reload or an edit that holds it.
    pub(crate) async fn setup(&self) -> Setup<'_> {
        Setup(self.setup.lock().await)
    }

    /// The reload, under the setup lock `held`: what runs stops as a retirement stops it (a start aborted, or the
    /// delivery ended and the service stopped) and its owner is waited for; then the boot's load on the blocking pool
    /// (it canonicalizes on the shared home), and, under the phase lock and only while the chat is not closed, the
    /// configuration swapped and the boot's launch; then `started` (the product's starts the owner). A closed chat
    /// stops, reads and swaps nothing. Answers the status the start ended in, or the status at [`SETUP_WAIT`]; a stop
    /// that outlasts it starts nothing. Each give-up is logged.
    async fn reload_then(self: &Arc<Self>, held: &Setup<'_>, started: impl FnOnce(&Arc<Self>)) -> Status {
        assert!(std::ptr::eq(tokio::sync::MutexGuard::mutex(&held.0), &self.setup), "a reload under another chat's setup lock");
        if self.shared.closed_now() {
            return self.status();
        }
        let stopped = async {
            self.stop_as(Status::Stopped);
            self.stopped().await;
            self.hub.owner_ended().await;
        };
        if tokio::time::timeout(SETUP_WAIT, stopped).await.is_err() {
            tracing::warn!("project chat: a reload gave up waiting for the chat to stop and started nothing");
            return self.status();
        }
        let env = self.env.clone();
        let Ok(load) = tokio::task::spawn_blocking(move || Self::load(&env, &config::read_file)).await else {
            tracing::warn!("project chat: a reload's load did not finish and started nothing");
            return self.status();
        };
        {
            let mut phase = self.shared.lock();
            if !self.shared.closed() {
                *self.config.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(load.config);
                *phase = Phase::Off(load.off);
                if let Some((credentials, locks)) = load.start {
                    self.launch(&mut phase, credentials, locks);
                }
            }
        }
        started(self);
        if tokio::time::timeout(SETUP_WAIT, self.running()).await.is_err() {
            tracing::warn!("project chat: a reload stopped waiting for its start, which goes on");
        }
        self.status()
    }

    /// Until the phase is `Off`.
    async fn stopped(&self) {
        loop {
            let changed = self.shared.started.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if matches!(*self.shared.lock(), Phase::Off(_)) {
                return;
            }
            changed.await;
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
            if let Some(code) = service.app_unchecked() {
                tracing::warn!(code, "project chat: the bot's app is unknown, so no frame is held to it");
            }
            // The mapped channels' names, looked up once in the background (`Api::channel_names`, which holds the
            // lookup to a count and a deadline): `chat.resolve` makes no Slack call and carries what the cache holds, so a
            // window's title can name the channel. A channel that got no name keeps its id, and its code is logged.
            if let (Some(Ok(config)), Ok(api)) = (chat.config().as_ref(), service.api()) {
                let channels: Vec<String> = config.mappings().map(|(_, channel)| channel.to_string()).collect();
                drop(tokio::spawn(async move {
                    for (channel, code) in api.channel_names(&channels).await {
                        tracing::warn!(code, channel, "project chat: a channel's name was not found");
                    }
                }));
            }
            if let Err(code) = chat.hub.ensure_owner(&service, &chat, &rows) {
                tracing::warn!(?code, "project chat owner did not start");
                // An ended service gives no `Watches`, so no owner would ever see its end.
                chat.service_ended(&service);
            }
        })
    }

    /// A service that ended on its own (`Service::ended`) turns the chat off with its code, logged once; a service that
    /// runs, or was stopped, changes nothing. Called by the owner as it ends and by the boot's task when no owner could
    /// start; the stop that follows asks no owner for its delivery (the owner is gone or ending), and the lock is freed
    /// with the service's task.
    pub(crate) fn service_ended(&self, service: &Service) {
        let Some(code) = service.ended() else { return };
        if self.stop_as(Status::Failed(code)) {
            tracing::warn!(code, "project chat: the chat service ended");
        }
    }

    /// The daemon's chat as the boot makes it, and as the tests' rig makes it: begin, the boot's log line, and the
    /// owner's task, started while the chat is still `Starting`. `verify` is the connect's checks (`Verifier::product`;
    /// a test's reach the fake Slack). `check` is [`Chat::begin`]'s; `timing` is the delivery's (`Timing::PRODUCT`; a test shortens it). Must run inside the daemon's
    /// runtime.
    #[allow(clippy::too_many_arguments, reason = "the boot's inputs, each a seam a test replaces")]
    pub(crate) fn boot(env: &Env, rows: &Workspaces, read: Reader, registry: &Registry, start: Starter<Service>, verify: setup::Verifier, check: Duration, timing: Timing) -> Arc<Chat> {
        let chat = Chat::begin(env, read, registry, start, verify, check);
        chat.hub.set_timing(timing);
        tracing::info!(status = %chat.status(), "project chat");
        drop(chat.start_owner(rows.clone()));
        chat
    }

    /// Load both files again and start from them, with no daemon restart: [`Chat::reload_then`] with the owner's start.
    pub(crate) async fn reload(self: &Arc<Self>, held: &Setup<'_>, rows: &Workspaces) -> Status {
        self.reload_then(held, |chat| drop(chat.start_owner(rows.clone()))).await
    }
}

impl<L: Listener> Participant for Chat<L> {
    fn name(&self) -> &'static str {
        "chat"
    }

    /// The chat has no admission to close yet: a handler's `admit` refuses `unavailable` once the stop begins.
    fn close_admission(&self) {}

    /// Marks the chat closed under the phase lock first, so a reload's start, which looks for the mark under it, either
    /// came before and is ended here or begins nothing.
    fn request_stop(&self) {
        {
            let _phase = self.shared.lock();
            self.shared.closed.store(true, Ordering::SeqCst);
        }
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
