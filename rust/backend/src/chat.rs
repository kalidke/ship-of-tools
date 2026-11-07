//! The project chat in the daemon: the one place `sotd` names `sot-chat`. It decides whether this daemon is the
//! configured hub before it reads any chat file, builds the row facts chat's mappings are admitted against, starts the
//! one service off the boot path, and retires it through lifecycle's registry. Nothing here serves an op.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use sot_chat::config::{self, Config, ConfigError, Credentials, Folder, Reader, Resolution};
use sot_chat::http::Http;
use sot_chat::service::Service;
use sot_platform::host::env::Env;
use sot_protocol::ops::{ChatCode, ChatTarget};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use crate::lifecycle::retire::{Participant, Registry, RETIRE_WAIT};
use crate::rows::{Workspace, Workspaces};

/// How long the service's own stop may take before it aborts the listener: inside lifecycle's bound, so a retirement
/// that completes is reported complete.
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

/// How a start is made: from the admitted configuration and credentials and the lock folder. The product's is
/// [`start_service`].
pub(crate) type Starter<L> = Box<
    dyn for<'a> FnOnce(&'a Config, Credentials, &'a Path) -> Pin<Box<dyn Future<Output = Result<Arc<L>, &'static str>> + Send + 'a>>
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
            Status::Refused(e) => write!(f, "{e}"),
            Status::Secrets(ConfigError::Missing) => f.write_str("chat-secrets.toml is missing from the config folder"),
            Status::Secrets(e) => write!(f, "{e}"),
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
}

impl<L> Shared<L> {
    fn lock(&self) -> MutexGuard<'_, Phase<L>> {
        self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_off(&self, status: Status) {
        *self.lock() = Phase::Off(status);
        self.off.notify_all();
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

/// The daemon's chat. Decided once, at construction: a daemon that is not the hub holds no configuration and no
/// service, and never reads a chat file again.
pub(crate) struct Chat<L: Listener = Service> {
    /// `None` on a daemon that is not the hub.
    config: Option<Result<Config, ConfigError>>,
    shared: Arc<Shared<L>>,
    /// The runtime that stops the service; `None` only for a chat that never started one.
    handle: Option<Handle>,
}

impl<L: Listener> Chat<L> {
    /// Admit, then start. In order: the hub fact; `chat.toml` against `rows`; `chat-secrets.toml` (the only open of it
    /// in the daemon, through `read`); the lock folder; registration with `registry`; then `start` on a task of its
    /// own, so boot never waits on Slack. A missing `chat.toml` leaves the chat off and writes nothing. Rows made later
    /// are not seen: a mapping to a folder no row had at this call is refused as `UnknownFolder` until the daemon
    /// restarts. Must run inside the daemon's runtime.
    pub(crate) fn begin(env: &Env, rows: &[RowFact], read: Reader, registry: &Registry, start: Starter<L>) -> Arc<Chat<L>> {
        let shared = Arc::new(Shared { phase: Mutex::new(Phase::Off(Status::NotHub)), off: Condvar::new() });
        let chat = |config, status| {
            *shared.lock() = Phase::Off(status);
            Arc::new(Chat { config, shared: shared.clone(), handle: Some(Handle::current()) })
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
        let locks = sot_platform::host::state_dir::daemon_state_dir_in(env).join("chat");
        if sot_platform::host::paths::ensure_private_dir(&locks).is_err() {
            return chat(Some(config), Status::Failed("lock"));
        }
        let this = chat(Some(config), Status::Stopped);
        // The phase lock is taken before the registration and held until `Starting` is in place: a retirement that
        // reaches `request_stop` in between waits for it, and so finds the start and aborts it. The start task's own
        // completion takes the same lock.
        let (task_chat, shared) = (this.clone(), this.shared.clone());
        let mut phase = shared.lock();
        if !registry.register(this.clone()) {
            return this;
        }
        let task = Handle::current().spawn(async move {
            let Some(Ok(config)) = &task_chat.config else { return };
            let made = start(config, credentials, &locks).await;
            let shared = &task_chat.shared;
            let mut phase = shared.lock();
            match (made, matches!(*phase, Phase::Starting(_))) {
                (Ok(l), true) => *phase = Phase::Running(l),
                // Retired while the start was under way: the service it made ends at once.
                (Ok(l), false) => {
                    tokio::spawn(async move { l.stop(STOP_WITHIN).await });
                }
                (Err(code), true) => {
                    drop(phase);
                    tracing::warn!(code, "project chat did not start");
                    shared.set_off(Status::Failed(code));
                }
                (Err(_), false) => {}
            }
        });
        *phase = Phase::Starting(task.abort_handle());
        drop(phase);
        this
    }

    /// A chat that is not the hub, for a test's services.
    #[cfg(test)]
    pub(crate) fn off() -> Arc<Chat<L>> {
        let shared = Arc::new(Shared { phase: Mutex::new(Phase::Off(Status::NotHub)), off: Condvar::new() });
        Arc::new(Chat { config: None, shared, handle: Handle::try_current().ok() })
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
    #[cfg_attr(not(test), allow(dead_code, reason = "the chat handlers are the first caller"))]
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

    /// M3: the one door every chat action that reaches Slack or a file passes, at the moment it is admitted. The service is running; the row is
    /// still the registry's current entry for that id (a removed row, a replaced one under a new id and an id that
    /// never existed are `StaleRow`); it is not the inert anchor (`Unmapped`, as a folder the file does not map); and
    /// its current folder maps, in the configuration loaded at start, to exactly the expected workspace and channel
    /// (anything else is `StaleMapping`). Only then is the service handed out. Limit: the check is at admission, no
    /// rows lock is held across a Slack call, and a call already made is not recalled when its row is removed
    /// afterwards; a later action, a watch's next snapshot included, is admitted again.
    #[cfg_attr(not(test), allow(dead_code, reason = "the chat handlers are the first caller"))]
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
    #[cfg_attr(not(test), allow(dead_code, reason = "the chat handlers are the first caller"))]
    pub(crate) fn admit_unwatch(&self) -> Result<(), ChatCode> {
        self.service().map(|_| ()).ok_or(ChatCode::Unavailable)
    }
}

impl<L: Listener> Participant for Chat<L> {
    fn name(&self) -> &'static str {
        "chat"
    }

    /// The chat has no admission to close yet: a handler's `admit` refuses `unavailable` once the stop begins.
    fn close_admission(&self) {}

    fn request_stop(&self) {
        let mut phase = self.shared.lock();
        match std::mem::replace(&mut *phase, Phase::Stopping) {
            Phase::Starting(task) => {
                task.abort();
                drop(phase);
                self.shared.set_off(Status::Stopped);
            }
            Phase::Running(l) => {
                drop(phase);
                let shared = self.shared.clone();
                let stop = async move {
                    l.stop(STOP_WITHIN).await;
                    shared.set_off(Status::Stopped);
                };
                match &self.handle {
                    Some(h) => drop(h.spawn(stop)),
                    None => unreachable!("a running service was started on a runtime"),
                }
            }
            other => *phase = other,
        }
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
mod tests;
