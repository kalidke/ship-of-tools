//! The project chat in the daemon: the one place `sotd` names `sot-chat`. It decides whether this daemon is the
//! configured hub before it reads any chat file, admits chat's mappings against the disk, starts one service per
//! connected Slack workspace off the boot path (a unit each, so one workspace's failure stops no other), reloads both
//! files and starts again on a change, and retires every unit through lifecycle's registry. `ops` serves the request
//! ops.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

use sot_chat::api::Transfers;
use sot_chat::config::{self, Config, ConfigError, Credentials, Reader, Resolution, Secrets};
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
pub(crate) mod setup;
pub(crate) mod watch;

use crate::lifecycle::retire::{Participant, Registry, RETIRE_WAIT};
use crate::rows::{Workspace, Workspaces};

/// How long the service's own stop may take before it aborts the listener: inside lifecycle's bound, so a stop with no
/// delivery to end is reported complete. A filing in flight is waited for first (`Hub::end_mail`, up to 15 s), and a
/// retirement that outlasts its bound says so. Every unit's stop runs at once, so the bound holds for all of them.
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

/// How a start is made: from one workspace's id, its credentials and the lock folder, once per try (a start refused
/// `held` is tried again, and a reload starts again). The product's is [`start_service`].
pub(crate) type Starter<L> = Box<
    dyn for<'a> Fn(&'a str, Credentials, &'a Path) -> Pin<Box<dyn Future<Output = Result<Arc<L>, &'static str>> + Send + 'a>>
        + Send
        + Sync,
>;

/// The product's starter. Every service it starts draws on one `Transfers`, so the chat it is given to holds at most
/// `MAX_TRANSFERS` files in memory whatever the number of its workspaces (C9); each start has its own `Http`, so one
/// workspace's hung requests never take another's operations.
pub(crate) fn start_service() -> Starter<Service> {
    let transfers = Transfers::new();
    Box::new(move |workspace, credentials, dir| {
        let (workspace, dir, transfers) = (workspace.to_string(), dir.to_path_buf(), transfers.clone());
        Box::pin(async move {
            let http = Http::new().map_err(|e| e.code())?;
            Service::start(&workspace, credentials, http, &dir, transfers).await.map(Arc::new).map_err(|e| e.code())
        })
    })
}

/// Why the chat (or one workspace's) is not running here, or that it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Not the configured hub: no chat file was opened.
    NotHub,
    /// `chat.toml` is missing or refused (the hub's, so its mappings were admitted against this daemon's rows).
    Refused(ConfigError),
    /// The workspace's secrets file is missing or refused; read only after the hub and `chat.toml` were admitted.
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

/// One connected workspace's listener: today's phase machine, per workspace (C5).
struct Shared<L> {
    phase: Mutex<Phase<L>>,
    off: Condvar,
    /// Woken when a start ends, in `Running` or in `Off`, and when a stop ends.
    started: Notify,
    /// The launches so far, counted under the phase lock.
    launches: AtomicU64,
}

impl<L> Shared<L> {
    fn new(off: Status) -> Arc<Self> {
        Arc::new(Shared { phase: Mutex::new(Phase::Off(off)), off: Condvar::new(), started: Notify::new(), launches: AtomicU64::new(0) })
    }

    fn lock(&self) -> MutexGuard<'_, Phase<L>> {
        self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_off(&self, status: Status) {
        *self.lock() = Phase::Off(status);
        self.off.notify_all();
        self.started.notify_waiters();
    }

    fn status(&self) -> Status {
        match &*self.lock() {
            Phase::Off(s) => *s,
            Phase::Starting(..) => Status::Starting,
            Phase::Running(_) => Status::Running,
            Phase::Stopping => Status::Stopping,
        }
    }

    fn service(&self) -> Option<Arc<L>> {
        match &*self.lock() {
            Phase::Running(l) => Some(l.clone()),
            _ => None,
        }
    }

    /// The running service once the start has ended: `None` when it ended any other way.
    async fn running(&self) -> Option<Arc<L>> {
        loop {
            let changed = self.started.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match &*self.lock() {
                Phase::Starting(..) => {}
                Phase::Running(l) => return Some(l.clone()),
                _ => return None,
            }
            changed.await;
        }
    }

    /// Until the phase is `Off`.
    async fn stopped(&self) {
        loop {
            let changed = self.started.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if matches!(*self.lock(), Phase::Off(_)) {
                return;
            }
            changed.await;
        }
    }
}

/// What the last load gave beside the configuration, and the units it made, swapped together under one lock. Lock
/// order: this lock, then a unit's phase lock, never the reverse (a start's completion takes its phase lock alone).
struct Units<L> {
    /// The file-level status: `NotHub`, `Refused`, `Failed("lock")`, or `Stopped` when each workspace's own decides.
    off: Status,
    /// Each workspace whose own secrets file was refused, with that file's code.
    refused: BTreeMap<String, ConfigError>,
    /// One unit per admitted workspace, in workspace order.
    map: BTreeMap<String, Arc<Shared<L>>>,
    /// No start may begin: the chat never registered with lifecycle (not the hub at its boot, or its registration
    /// refused) or its retirement began. Set and read under this lock.
    closed: bool,
}

/// The one watch on the hub fact, the chat's (not a workspace's): each `check`, while any unit runs or starts, it asks the
/// topology and, when another computer is named the hub, stops every unit (freeing each lock) as `NotHub`. It holds the
/// chat only across one ask, so a dropped chat ends it, and it ends with the chat's retirement.
async fn step_down_when_another_hub<L: Listener>(chat: Weak<Chat<L>>, env: Env, check: Duration) {
    loop {
        tokio::time::sleep(check).await;
        let Some(chat) = chat.upgrade() else { return };
        let live: Vec<(String, Arc<Shared<L>>)> = {
            let units = chat.units();
            if units.closed {
                return;
            }
            units.map.iter().filter(|(_, u)| matches!(*u.lock(), Phase::Running(_) | Phase::Starting(..))).map(|(w, u)| (w.clone(), u.clone())).collect()
        };
        if live.is_empty() {
            continue;
        }
        let e = env.clone();
        if tokio::task::spawn_blocking(move || names_another_hub(&e)).await.unwrap_or(false) {
            tracing::warn!("project chat is off: the topology names another computer as the hub");
            for (w, u) in &live {
                chat.stop_unit(u, w, Status::NotHub);
            }
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
    /// The file-level status: `Stopped` when each admitted workspace may start.
    off: Status,
    /// Each admitted workspace's credentials, in workspace order.
    plans: Vec<(String, Credentials)>,
    /// The lock folder, when the files were admitted.
    locks: Option<PathBuf>,
    /// Each workspace whose own secrets file was refused, with that file's code.
    refused: BTreeMap<String, ConfigError>,
    /// The ids of the named secrets files the folder lists, the default's left out (a connect's bound).
    named: Vec<String>,
}

/// One unit per admitted workspace, each `Off(Stopped)` until it is launched.
fn units_of<L>(plans: &[(String, Credentials)]) -> BTreeMap<String, Arc<Shared<L>>> {
    plans.iter().map(|(w, _)| (w.clone(), Shared::new(Status::Stopped))).collect()
}

/// The daemon's chat. Decided at construction: a daemon that is not the hub at its boot holds no configuration and no
/// service, and never reads a chat file again (a hub that stepped down reads both files again at its next reload). Once
/// it runs, one watch asks the topology each [`HUB_CHECK`] and steps every workspace down when another computer is
/// named the hub.
pub(crate) struct Chat<L: Listener = Service> {
    /// What the last load gave (`None` on a daemon that is not the hub), swapped by a reload only while no service runs,
    /// so a service and its owner see one configuration for their life.
    config: Mutex<Arc<Option<Result<Config, ConfigError>>>>,
    units: Mutex<Units<L>>,
    /// The last name Slack gave this daemon for each admitted workspace (a start's `auth.test`, a connect's check), kept
    /// past its service's end, so a failed workspace is still named; pruned to the admitted workspaces at every load.
    names: Mutex<BTreeMap<String, String>>,
    /// The runtime that stops the services; `None` only for a chat that never started one.
    handle: Option<Handle>,
    /// Which connection watches which channel of which workspace, and each workspace's owner task.
    hub: Arc<watch::Hub>,
    /// The environment `begin` was given: the owners' state folder and every filing are read through it.
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
    /// The one load, the boot's and each reload's, and the only caller of `Config::load` and `Secrets::read`. In
    /// order: the hub fact; `chat.toml` against the disk; the secrets files (the only opens of them in the daemon, through
    /// `read`), each admitted workspace a plan and each refused file logged by its name; the lock folder (the comm
    /// folder's `chat/`). A missing `chat.toml` leaves the chat off and writes nothing. A mapping needs no row: one whose
    /// folder does not exist is ignored and logged by its ordinal, and a row made later in a mapped folder is admitted
    /// at its first action.
    fn load(env: &Env, read: Reader) -> Load {
        let off = |config, off| Load { config, off, plans: Vec::new(), locks: None, refused: BTreeMap::new(), named: Vec::new() };
        if !is_hub(env) {
            return off(None, Status::NotHub);
        }
        let Some(dir) = config::config_dir(env) else {
            return off(Some(Err(ConfigError::Missing)), Status::Refused(ConfigError::Missing));
        };
        let config = Config::load(&dir, read, &canon_of);
        let admitted = match &config {
            Err(e) => {
                let refused = Status::Refused(*e);
                return off(Some(config), refused);
            }
            Ok(admitted) => admitted,
        };
        for n in admitted.ignored() {
            tracing::warn!(mapping = n, "project chat: a mapping's folder does not exist and is ignored");
        }
        let mut secrets = Secrets::read(&dir, read, admitted);
        for (file, why) in secrets.refused() {
            tracing::warn!(file = %file, %why, "project chat: a secrets file is not served");
        }
        let mut refused = BTreeMap::new();
        let named = secrets.named(admitted.workspace());
        for (workspace, why) in secrets.refusals(admitted.workspace()) {
            refused.entry(workspace).or_insert(why);
        }
        let ids: Vec<String> = secrets
            .workspaces()
            .map(|(w, file, credentials)| {
                if credentials.user.is_none() {
                    tracing::warn!("{} holds no user token: a person's post waits for Connect Slack in the Chat drawer", file.name());
                }
                w.to_string()
            })
            .collect();
        let plans: Vec<(String, Credentials)> = ids.into_iter().filter_map(|w| secrets.take(&w).map(|c| (w, c))).collect();
        drop(secrets);
        // The comm folder's, not this daemon's: every daemon that files into these inboxes takes the same lock.
        let Some(locks) = crate::comm::home::chat_dir(env) else {
            return Load { config: Some(config), off: Status::Failed("lock"), plans, locks: None, refused, named };
        };
        Load { config: Some(config), off: Status::Stopped, plans, locks: Some(locks), refused, named }
    }

    /// Load, register, then start every admitted workspace. A hub registers with `registry` whatever its load gave, so a
    /// start a reload makes later retires with the daemon; a daemon that is not the hub registers nothing and never
    /// starts. Each start runs on a task of its own, so boot never waits on Slack. `check` is the pace of the retry of a
    /// start refused `held` and of the hub fact's asks (`HUB_CHECK`; the tests shorten it). Must run inside the
    /// daemon's runtime.
    fn begin(env: &Env, read: Reader, registry: &Registry, start: Starter<L>, verify: setup::Verifier, check: Duration) -> Arc<Chat<L>> {
        let load = Self::load(env, read);
        let hub = load.config.is_some();
        let units = Units { off: load.off, refused: load.refused, map: units_of(&load.plans), closed: !hub };
        let (config, handle, start) = (Mutex::new(Arc::new(load.config)), Some(Handle::current()), Arc::new(start));
        let this = Arc::new(Chat {
            config,
            units: Mutex::new(units),
            names: Default::default(),
            handle,
            hub: watch::Hub::new(),
            env: env.clone(),
            start,
            verify,
            check,
            setup: Default::default(),
        });
        if !hub {
            return this;
        }
        // The units lock is taken before the registration and held until every start is in place: a retirement that
        // reaches `request_stop` in between waits for it, and so finds each start and aborts it. A start task's own
        // completion takes its unit's phase lock alone.
        let mut units = this.units();
        if !registry.register(this.clone()) {
            units.closed = true;
        } else {
            drop(tokio::spawn(step_down_when_another_hub(Arc::downgrade(&this), env.clone(), check)));
            if let Some(locks) = load.locks {
                for (workspace, credentials) in load.plans {
                    this.launch(&units, &workspace, credentials, locks.clone());
                }
            }
        }
        drop(units);
        this
    }

    fn units(&self) -> MutexGuard<'_, Units<L>> {
        self.units.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether no start may begin (read under the units lock).
    fn closed_now(&self) -> bool {
        self.units().closed
    }

    /// Every unit, with its workspace, as the units lock holds them now.
    fn unit_list(&self) -> Vec<(String, Arc<Shared<L>>)> {
        self.units().map.iter().map(|(w, u)| (w.clone(), u.clone())).collect()
    }

    fn unit(&self, workspace: &str) -> Option<Arc<Shared<L>>> {
        self.units().map.get(workspace).cloned()
    }

    /// The configuration of the last load.
    fn config(&self) -> Arc<Option<Result<Config, ConfigError>>> {
        self.config.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `chat.toml`'s workspace, the one a request naming none means (C4); `None` when no `chat.toml` is admitted.
    pub(crate) fn default_workspace(&self) -> Option<String> {
        match self.config().as_ref() {
            Some(Ok(c)) => Some(c.workspace().to_string()),
            _ => None,
        }
    }

    /// The start of `workspace`'s unit, on a task, its phase then `Starting`: tried again each `check` while refused
    /// `held`, then `Running`, or off with its status. The caller holds the units lock and has found the chat not
    /// closed under it.
    fn launch(self: &Arc<Self>, units: &Units<L>, workspace: &str, credentials: Credentials, locks: PathBuf) {
        let Some(unit) = units.map.get(workspace).cloned() else { return };
        let mut phase = unit.lock();
        let (env, check, start, shared) = (self.env.clone(), self.check, self.start.clone(), unit.clone());
        let launch = unit.launches.fetch_add(1, Ordering::SeqCst);
        let workspace = workspace.to_string();
        let task = Handle::current().spawn(async move {
            let mut held = false;
            // A start refused `held` is tried again each `check` until a readable topology names another computer the
            // hub (a missing or unparsable one, as an edit in progress leaves, ends nothing), the phase staying
            // `Starting` so `running` keeps waiting. The credentials are cloned per try and never formatted.
            let made = loop {
                match start(&workspace, credentials.clone(), &locks).await {
                    Err("held") => {
                        if !std::mem::replace(&mut held, true) {
                            tracing::warn!(workspace, "project chat is held by another daemon on this comm folder; trying again");
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
            let mut phase = shared.lock();
            match (made, matches!(*phase, Phase::Starting(_, n) if n == launch)) {
                (Ok(l), true) => {
                    *phase = Phase::Running(l);
                    shared.started.notify_waiters();
                }
                // Retired while the start was under way: the service it made ends at once.
                (Ok(l), false) => {
                    tokio::spawn(async move { l.stop(STOP_WITHIN).await });
                }
                (Err(status), true) => {
                    // Off under the lock that found this launch current: a later launch's `Starting` is never
                    // overwritten.
                    *phase = Phase::Off(status);
                    drop(phase);
                    shared.off.notify_all();
                    shared.started.notify_waiters();
                    match status {
                        Status::Failed(code) => tracing::warn!(workspace, code, "project chat: a workspace's chat did not start"),
                        _ => tracing::warn!(workspace, "project chat is off: the topology names another computer as the hub"),
                    }
                }
                (Err(_), false) => {}
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
        let units = Mutex::new(Units { off: Status::NotHub, refused: BTreeMap::new(), map: BTreeMap::new(), closed: true });
        let (config, handle) = (Mutex::new(Arc::new(None)), Handle::try_current().ok());
        let start: Arc<Starter<L>> = Arc::new(Box::new(|_, _, _| Box::pin(std::future::pending())));
        Arc::new(Chat {
            config,
            units,
            names: Default::default(),
            handle,
            hub: watch::Hub::new(),
            env: Env::empty(),
            start,
            verify: setup::Verifier::none(),
            check: HUB_CHECK,
            setup: Default::default(),
        })
    }

    pub(crate) fn hub(&self) -> &Arc<watch::Hub> {
        &self.hub
    }

    /// The default workspace's status (`chat.toml`'s), as a one-workspace hub's chat has always answered.
    pub(crate) fn status(&self) -> Status {
        match self.default_workspace() {
            Some(w) => self.status_of(&w),
            None => self.units().off,
        }
    }

    /// One workspace's status: the file-level status when the chat is off at file level (not the hub, a refused or
    /// missing `chat.toml`, no lock folder); else its unit's; else, with no admitted file for it, `Secrets` with its own
    /// file's refusal, or `Missing`. A named file's refusal is its own workspace's, never the default's.
    pub(crate) fn status_of(&self, workspace: &str) -> Status {
        let units = self.units();
        if units.off != Status::Stopped {
            return units.off;
        }
        match units.map.get(workspace) {
            Some(u) => u.status(),
            None => Status::Secrets(units.refused.get(workspace).copied().unwrap_or(ConfigError::Missing)),
        }
    }

    /// Whether any workspace's listener runs.
    pub(crate) fn any_running(&self) -> bool {
        self.units().map.values().any(|u| matches!(*u.lock(), Phase::Running(_)))
    }

    /// `chat.toml`'s workspace and every connected workspace with its last name and whether it runs, for
    /// `chat.workspaces`; `None` while no `chat.toml` is admitted.
    pub(crate) fn workspaces(&self) -> Option<(String, Vec<(String, Option<String>, bool)>)> {
        let default = self.default_workspace()?;
        let names = self.names.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let units = self.units();
        let list = units.map.iter().map(|(w, u)| (w.clone(), names.get(w).cloned(), matches!(*u.lock(), Phase::Running(_)))).collect();
        Some((default, list))
    }

    /// Keeps `name` as the last one Slack gave for `workspace`, when the workspace is admitted.
    pub(crate) fn remember_name(&self, workspace: &str, name: Option<String>) {
        let admitted = self.units().map.contains_key(workspace);
        if let (true, Some(name)) = (admitted, name) {
            self.names.lock().unwrap_or_else(|e| e.into_inner()).insert(workspace.to_string(), name);
        }
    }

    /// The running service of `workspace`. Private: a chat action reaches it only through [`Chat::admit`].
    fn service_of(&self, workspace: &str) -> Option<Arc<L>> {
        self.unit(workspace)?.service()
    }

    /// The default workspace's running service.
    #[cfg(test)]
    fn service(&self) -> Option<Arc<L>> {
        self.service_of(&self.default_workspace()?)
    }

    /// `workspace`'s running service once its start has ended: `None` when it ended any other way (never the hub,
    /// refused, failed, retired) or the workspace has no unit.
    async fn running_of(&self, workspace: &str) -> Option<Arc<L>> {
        self.unit(workspace)?.running().await
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
    /// takes its workspace's running service. Not the hub: `NotHub`. A row that is gone: `StaleRow`. The inert anchor:
    /// `Unmapped`. Every status shows the file's mapping, and only its workspace's `Running` delivers; a refused
    /// `chat.toml` is `Invalid`.
    pub(crate) fn resolve(&self, rows: &Workspaces, row: &str) -> Result<(Resolution, bool), ChatCode> {
        if self.config().is_none() {
            return Ok((Resolution::NotHub, false));
        }
        let fact = current_row(rows, row).ok_or(ChatCode::StaleRow)?;
        if fact.inert {
            return Ok((Resolution::Unmapped, false));
        }
        let resolution = self.mapping(&fact.canonical.to_string_lossy());
        let delivers = match &resolution {
            Resolution::Mapped { workspace, .. } => resolution.delivers() && self.status_of(workspace) == Status::Running,
            _ => false,
        };
        Ok((resolution, delivers))
    }

    /// M3: the one door every chat action that reaches Slack or a file passes, at the moment it is admitted. The target's
    /// workspace's service is running; the row is still the registry's current entry for that id (a removed row, a
    /// replaced one under a new id and an id that never existed are `StaleRow`); it is not the inert anchor
    /// (`Unmapped`, as a folder the file does not map); and its current folder maps, in the configuration of the last
    /// load, to exactly the expected workspace and channel (anything else is `StaleMapping`). Only then is that
    /// workspace's service handed out, so no row reaches Slack under another workspace's token. Limit: the check is at
    /// admission, no rows lock is held across a Slack call, and a call already made is not recalled when its row is
    /// removed afterwards; a later action, a watch's next snapshot included, is admitted again.
    pub(crate) fn admit(&self, rows: &Workspaces, target: &ChatTarget) -> Result<Arc<L>, ChatCode> {
        let (service, fact) = self.admit_row(rows, &target.row, &target.workspace)?;
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

    /// [`Chat::admit`] without the mapping check, for a pairing: the row may map to nothing yet. `workspace`'s service is
    /// running and the row is the registry's current entry and not the inert anchor; answers the service and the row's
    /// folder.
    pub(crate) fn admit_row(&self, rows: &Workspaces, row: &str, workspace: &str) -> Result<(Arc<L>, RowFact), ChatCode> {
        let service = self.service_of(workspace).ok_or(ChatCode::Unavailable)?;
        let fact = current_row(rows, row).ok_or(ChatCode::StaleRow)?;
        if fact.inert {
            return Err(ChatCode::Unmapped);
        }
        Ok((service, fact))
    }

    /// The door for an unwatch, which hands out no service: it ends access and calls nothing, so it needs no current
    /// row, and refusing it for a stale one would leave the watch holding one of the bounded slots.
    pub(crate) fn admit_unwatch(&self, workspace: &str) -> Result<(), ChatCode> {
        self.service_of(workspace).map(|_| ()).ok_or(ChatCode::Unavailable)
    }

    /// End `workspace`'s start or service, the unit then `Off(ended)`: `Stopped` for a retirement or a reload, `NotHub`
    /// for a step-down, `Failed(code)` for a service that ended on its own. Whether this call began the end: a unit
    /// already stopping or off is left as it is. Its stop runs on a task of its own, so stops of several units run at
    /// once.
    fn stop_unit(&self, unit: &Arc<Shared<L>>, workspace: &str, ended: Status) -> bool {
        let mut phase = unit.lock();
        match std::mem::replace(&mut *phase, Phase::Stopping) {
            Phase::Starting(task, _) => {
                task.abort();
                drop(phase);
                unit.set_off(ended);
                true
            }
            Phase::Running(l) => {
                drop(phase);
                let (unit, hub, workspace) = (unit.clone(), self.hub.clone(), workspace.to_string());
                let stop = async move {
                    // The delivery ends first, its filing in flight returned, while the lock is still held.
                    hub.end_mail(&workspace).await;
                    l.stop(STOP_WITHIN).await;
                    unit.set_off(ended);
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

    /// Every unit's end, each as [`Chat::stop_unit`], all begun at once.
    fn stop_all(&self, ended: Status) {
        for (w, u) in self.unit_list() {
            self.stop_unit(&u, &w, ended);
        }
    }
}

/// How long a reload waits for what runs to stop, and then for its starts to end.
pub(crate) const SETUP_WAIT: Duration = Duration::from_secs(30);

/// The setup lock, held: one reload, or one edit of the files and the reload after it, at a time. Made only by
/// [`Chat::setup`], and a reload takes it, so a holder edits the files and reloads with no second lock to deadlock on.
pub(crate) struct Setup<'a>(tokio::sync::MutexGuard<'a, ()>);

impl<L: Listener> Chat<L> {
    /// Take the setup lock, waiting for a reload or an edit that holds it.
    pub(crate) async fn setup(&self) -> Setup<'_> {
        Setup(self.setup.lock().await)
    }

    /// The reload, under the setup lock `held`. Until reloads are per workspace (`CHAT-RELOAD-RESTARTS-EVERY-WORKSPACE`)
    /// it restarts every workspace: every unit stops as a retirement stops it (a start aborted, or the delivery ended and
    /// the service stopped), all at once, and each unit and its owner is waited for; then the boot's load on the blocking
    /// pool (it canonicalizes on the shared home), and, under the units lock and only while the chat is not closed, the
    /// configuration and the units swapped and each admitted workspace launched; then `started` (the product's starts
    /// the owners). A closed chat stops, reads and swaps nothing. Returns once every start ended, or at [`SETUP_WAIT`]; a
    /// stop that outlasts it starts nothing. Each give-up is logged; a caller reads each workspace's status after it.
    async fn reload_then(self: &Arc<Self>, held: &Setup<'_>, started: impl FnOnce(&Arc<Self>)) {
        assert!(std::ptr::eq(tokio::sync::MutexGuard::mutex(&held.0), &self.setup), "a reload under another chat's setup lock");
        if self.closed_now() {
            return;
        }
        let all = self.unit_list();
        let stopped = async {
            for (w, u) in &all {
                self.stop_unit(u, w, Status::Stopped);
            }
            for (w, u) in &all {
                u.stopped().await;
                self.hub.owner_ended(w).await;
            }
        };
        if tokio::time::timeout(SETUP_WAIT, stopped).await.is_err() {
            tracing::warn!("project chat: a reload gave up waiting for the chat to stop and started nothing");
            return;
        }
        let env = self.env.clone();
        let Ok(load) = tokio::task::spawn_blocking(move || Self::load(&env, &config::read_file)).await else {
            tracing::warn!("project chat: a reload's load did not finish and started nothing");
            return;
        };
        {
            let mut units = self.units();
            if !units.closed {
                *self.config.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(load.config);
                (units.off, units.refused, units.map) = (load.off, load.refused, units_of(&load.plans));
                self.names.lock().unwrap_or_else(|e| e.into_inner()).retain(|w, _| units.map.contains_key(w));
                if let Some(locks) = load.locks {
                    for (workspace, credentials) in load.plans {
                        self.launch(&units, &workspace, credentials, locks.clone());
                    }
                }
            }
        }
        started(self);
        let starts = async {
            for (_, u) in self.unit_list() {
                u.running().await;
            }
        };
        if tokio::time::timeout(SETUP_WAIT, starts).await.is_err() {
            tracing::warn!("project chat: a reload stopped waiting for its start, which goes on");
        }
    }

    /// Until `workspace`'s unit is `Off` (none: at once).
    async fn stopped_of(&self, workspace: &str) {
        if let Some(u) = self.unit(workspace) {
            u.stopped().await;
        }
    }
}

impl Chat<Service> {
    /// The boot's one task: for each workspace, when its service is running, log it once and start its owner, which
    /// serves the socket with no window open. `ensure_owner` stays the one start; a watch that comes first starts it
    /// too. One workspace's slow start holds no other's owner.
    pub(crate) fn start_owner(self: &Arc<Self>, rows: Workspaces) -> tokio::task::JoinHandle<()> {
        let chat = self.clone();
        tokio::spawn(async move {
            let mut owners = tokio::task::JoinSet::new();
            for (workspace, _) in chat.unit_list() {
                let (chat, rows) = (chat.clone(), rows.clone());
                owners.spawn(async move { chat.own(&workspace, &rows).await });
            }
            while owners.join_next().await.is_some() {}
        })
    }

    /// One workspace's part of [`Chat::start_owner`].
    async fn own(self: &Arc<Self>, workspace: &str, rows: &Workspaces) {
        let Some(service) = self.running_of(workspace).await else { return };
        tracing::info!(status = %Status::Running, workspace, "project chat");
        if let Some(code) = service.app_unchecked() {
            tracing::warn!(code, workspace, "project chat: the bot's app is unknown, so no frame is held to it");
        }
        if let Some(code) = service.person_dropped() {
            tracing::warn!(code, workspace, "project chat: the saved user token is not this workspace's, so a person's post waits for Connect Slack");
        }
        self.remember_name(workspace, service.workspace_name());
        // The mapped channels' names, looked up once in the background (`Api::channel_names`, which holds the
        // lookup to a count and a deadline): `chat.resolve` makes no Slack call and carries what the cache holds, so a
        // window's title can name the channel. A channel that got no name keeps its id, and its code is logged.
        if let (Some(Ok(config)), Ok(api)) = (self.config().as_ref(), service.api()) {
            let channels: Vec<String> = config.mappings().filter(|(_, w, _)| *w == workspace).map(|(_, _, channel)| channel.to_string()).collect();
            drop(tokio::spawn(async move {
                for (channel, code) in api.channel_names(&channels).await {
                    tracing::warn!(code, channel, "project chat: a channel's name was not found");
                }
            }));
        }
        if let Err(code) = self.hub.ensure_owner(&service, self, rows) {
            tracing::warn!(?code, workspace, "project chat owner did not start");
        }
    }

    /// The running `service`, if it ended on its own (`Service::ended`), turns its workspace's unit off with its code,
    /// logged once when this call began the end; a service that runs, was stopped or is not the one its unit runs
    /// changes nothing. Called by the owner as it ends and by `Hub::ensure_owner` when an ended service gives no
    /// `Watches`, so no owner would ever see its end. The listener's task, which owns the lock, is already gone.
    fn service_ended(&self, service: &Service) {
        let Some(code) = service.ended() else { return };
        let workspace = service.workspace();
        let Some(unit) = self.unit(workspace) else { return };
        if !matches!(&*unit.lock(), Phase::Running(l) if std::ptr::eq(Arc::as_ptr(l), service)) {
            return;
        }
        if self.stop_unit(&unit, workspace, Status::Failed(code)) {
            tracing::warn!(code, workspace, "project chat: a workspace's chat service ended");
        }
    }

    /// The daemon's chat as the boot makes it, and as the tests' rig makes it: begin, the boot's log line, and the
    /// owners' task, started while the chat is still `Starting`. `verify` is the connect's checks (`Verifier::product`;
    /// a test's reach the fake Slack). `check` is [`Chat::begin`]'s; `timing` is the delivery's (`Timing::PRODUCT`; a
    /// test shortens it). Must run inside the daemon's runtime.
    #[allow(clippy::too_many_arguments, reason = "the boot's inputs, each a seam a test replaces")]
    pub(crate) fn boot(env: &Env, rows: &Workspaces, read: Reader, registry: &Registry, start: Starter<Service>, verify: setup::Verifier, check: Duration, timing: Timing) -> Arc<Chat> {
        let chat = Chat::begin(env, read, registry, start, verify, check);
        chat.hub.set_timing(timing);
        tracing::info!(status = %chat.status(), "project chat");
        drop(chat.start_owner(rows.clone()));
        chat
    }

    /// Load both files again and start from them, with no daemon restart: [`Chat::reload_then`] with the owners' start.
    pub(crate) async fn reload(self: &Arc<Self>, held: &Setup<'_>, rows: &Workspaces) {
        self.reload_then(held, |chat| drop(chat.start_owner(rows.clone()))).await
    }
}

impl<L: Listener> Participant for Chat<L> {
    fn name(&self) -> &'static str {
        "chat"
    }

    /// The chat has no admission to close yet: a handler's `admit` refuses `unavailable` once the stop begins.
    fn close_admission(&self) {}

    /// Marks the chat closed under the units lock first, so a reload's or the boot's start, which looks for the mark
    /// under it, either came before and is ended here or begins nothing; then every unit's stop begins, all at once.
    fn request_stop(&self) {
        self.units().closed = true;
        self.stop_all(Status::Stopped);
    }

    /// Whether every unit is `Off` within one deadline for them all.
    fn wait_retired(&self, within: Duration) -> bool {
        let end = std::time::Instant::now() + within;
        self.unit_list().iter().all(|(_, u)| {
            let phase = u.lock();
            let left = end.saturating_duration_since(std::time::Instant::now());
            let (phase, _) = u.off.wait_timeout_while(phase, left, |p| !matches!(p, Phase::Off(_))).unwrap_or_else(|e| e.into_inner());
            matches!(*phase, Phase::Off(_))
        })
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
pub(crate) mod tests;
