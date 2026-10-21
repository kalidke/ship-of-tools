//! The ssh child every non-local dial spawns (isolation-plan.md §3 C3, as
//! amended by `dev/output/c3-second-connection-amendment.md`). Reaching a
//! daemon that is not on this box now means `ssh <target> '<PATH prelude>;
//! sotd stdio-bridge [--host <host>]'`, piped stdio, no shell on THIS end —
//! never an `ssh -L` forward. One recipe type and one argv builder here,
//! used by every Rust site that spawns one:
//!
//! - the frontend's control connection (`rust/frontend/src/net/transport/mod.rs`);
//! - its per-host lane attach (`crate::topology::lane_client`, this crate);
//! - its per-browser-connection page-proxy leg
//!   (`rust/frontend/src/pages.rs`).
//!
//! C10's shell helper (`comm-lib.sh`'s `sot_ssh_bridge`) spawns the same
//! child from the comm scripts, with its own implementation — shell cannot
//! call into this crate, so that side owns its own copy of the option set
//! and prelude, kept identical by convention rather than by sharing code.

use std::process::Stdio;

use crate::topology::endpoint::is_plain_host_name;

/// Where an ssh child should connect, and — when that target is a hub
/// relaying on another daemon's behalf — which daemon. `target` is an ssh
/// destination (a `hosts.toml` hub, or a bare `--dial`-supplied name);
/// `host`, when set, is `ssh:<target>/<host>`'s own suffix, passed on to
/// `sotd stdio-bridge --host <host>` on the far end.
///
/// Constructible only through [`SshRecipe::new`] — never with the struct
/// literal from outside this module — so nothing downstream can hand
/// `argv` an unchecked half. `target` becomes ssh's own argv element (a
/// value beginning with `-` would otherwise be read as an ssh OPTION, e.g.
/// `-oProxyCommand=…`, run on THIS box); `host` is interpolated into the
/// remote command STRING a shell on the far end parses. Both are checked
/// against the same plain-host-name grammar `sotd topology plan` emits
/// hosts in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshRecipe {
    target: String,
    host: Option<String>,
    #[cfg(any(test, feature = "test-handshake-bound"))]
    test_command: Option<TestCommand>,
}

#[cfg(any(test, feature = "test-handshake-bound"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct TestCommand {
    program: std::path::PathBuf,
    args: Vec<std::ffi::OsString>,
}

impl SshRecipe {
    pub fn new(target: &str, host: Option<&str>) -> Result<Self, String> {
        if !is_plain_host_name(target) {
            return Err(format!("`{target}` is not a plain host name"));
        }
        if let Some(h) = host {
            if !is_plain_host_name(h) {
                return Err(format!("`{h}` is not a plain host name"));
            }
        }
        Ok(Self {
            target: target.to_string(),
            host: host.map(str::to_string),
            #[cfg(any(test, feature = "test-handshake-bound"))]
            test_command: None,
        })
    }

    /// Test builds only: every start this recipe drives runs `program` with `args` in place of `ssh`, through the same `LinkGate`.
    #[cfg(any(test, feature = "test-handshake-bound"))]
    pub fn with_test_command(mut self, program: impl Into<std::path::PathBuf>, args: impl IntoIterator<Item = impl Into<std::ffi::OsString>>) -> Self {
        self.test_command = Some(TestCommand { program: program.into(), args: args.into_iter().map(Into::into).collect() });
        self
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }
}

impl std::fmt::Display for SshRecipe {
    /// The same spelling `sotd topology plan` emits an `ssh:` endpoint in
    /// (`ssh:<target>` / `ssh:<target>/<host>`) — used for logging only;
    /// nothing re-parses this string.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.host {
            Some(h) => write!(f, "ssh:{}/{h}", self.target),
            None => write!(f, "ssh:{}", self.target),
        }
    }
}

/// The remote PATH prelude every spawned child's command string opens
/// with, so a non-interactive remote account whose shell doesn't source
/// the usual profile still finds `sotd` — the launchers' own
/// (`scripts/launch-sot.sh:229-231`; C10's shell helper spells the
/// identical string on its own side).
pub const PATH_PRELUDE: &str = r#"export PATH="$HOME/.local/share/sot/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH""#;

/// `ssh`'s own option set: the relay unit's `ExecStart` options (`crate::topology`), sharing off included
/// (`ControlMaster=no`, `ControlPath=none`, `ControlPersist=no`, so no master forks away from the tree the daemon
/// kills), plus `ConnectTimeout=10`, the bridge's own bound on a dead hub. The daemon's monitor starts its sampler's ssh
/// from the same list.
pub const SSH_OPTS: &[&str] = &["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", "-o", "ConnectTimeout=10", "-o", "ControlMaster=no", "-o", "ControlPath=none", "-o", "ControlPersist=no"];

/// How long a lane dial over ssh, or over a hub relay, waits for the far daemon's first reply. A whole login comes
/// before that reply (the connect, key exchange, authentication, the remote shell and `sotd stdio-bridge`), so the
/// local handshake bound (`CONNECT_BOUND`, 2 s) would kill a login that is merely slow, and its retry is one more login
/// on a busy host. The window's control connection and page proxy wait the same 30 s for the same login
/// (`CONNECT_REPLY_BOUND`).
pub const SSH_LOGIN_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// `recipe` → `ssh`'s own program name and argv, built with no shell on
/// this end: `target` is its own argv element (never folded into the
/// remote command string), the remote command is ONE further argv
/// element that `sotd stdio-bridge` command with `--host <host>` appended
/// only when the recipe carries one.
fn argv(recipe: &SshRecipe) -> (&'static str, Vec<String>) {
    let mut args: Vec<String> = SSH_OPTS.iter().map(|s| s.to_string()).collect();
    args.push(recipe.target.clone());
    let mut remote = format!("{PATH_PRELUDE}; sotd stdio-bridge");
    if let Some(h) = &recipe.host {
        remote.push_str(" --host ");
        remote.push_str(h);
    }
    args.push(remote);
    ("ssh", args)
}

/// The ssh command for `recipe`: its argv and three piped stdio.
fn command(recipe: &SshRecipe) -> std::process::Command {
    type Argv = (std::ffi::OsString, Vec<std::ffi::OsString>);
    #[cfg(any(test, feature = "test-handshake-bound"))]
    let fixture: Option<Argv> = recipe.test_command.as_ref().map(|f| (f.program.clone().into(), f.args.clone()));
    #[cfg(not(any(test, feature = "test-handshake-bound")))]
    let fixture: Option<Argv> = None;
    let (program, args) = fixture.unwrap_or_else(|| {
        let (program, args) = argv(recipe);
        (program.into(), args.into_iter().map(Into::into).collect())
    });
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    keep_no_descriptor(&mut cmd);
    cmd
}

/// Marks this process's descriptors close-on-exec through the one sweep the daemon's and the capsule's children use, so
/// the child keeps none past its exec. On macOS a descriptor is close-on-exec only after a second call, and the window
/// starts its ssh children from several threads, so one started between the two calls would keep it.
fn keep_no_descriptor(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the hook runs in the child between fork and exec and makes only async-signal-safe calls.
        unsafe { cmd.pre_exec(sot_platform::host::inherit_none) };
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// Spawn the child for a synchronous caller — the lane client, which
/// implements `sot_log::lane::client::Client`'s blocking `&self` methods and so
/// cannot hold a tokio `Child`.
fn spawn_sync(mut command: std::process::Command) -> std::io::Result<std::process::Child> {
    #[allow(clippy::disallowed_methods, reason = "the window's and the lane client's ssh; the daemon starts ssh only through LinkGate::command and lifecycle's containment")]
    let child = command.spawn();
    child
}

/// Spawn the child for an async/tokio caller — the frontend's control
/// connection and its page-proxy splice. `kill_on_drop` so a dropped
/// `Child` (the branch returning, the splice task ending) never leaves an
/// orphaned `ssh` running past its client — the async twin of
/// `BridgedClient`'s own explicit `Drop` on the sync side.
fn spawn_async(command: std::process::Command) -> std::io::Result<tokio::process::Child> {
    #[allow(clippy::disallowed_methods, reason = "the window's and the lane client's ssh; the daemon starts ssh only through LinkGate::command and lifecycle's containment")]
    let child = tokio::process::Command::from(command).kill_on_drop(true).spawn();
    child
}

/// Both gated command APIs return a real owned child before admission ends.
pub(crate) trait StartedChild {
    #[cfg(test)]
    fn child_id(&self) -> Option<u32>;
}
impl StartedChild for std::process::Child {
    #[cfg(test)]
    fn child_id(&self) -> Option<u32> { Some(self.id()) }
}
impl StartedChild for tokio::process::Child {
    #[cfg(test)]
    fn child_id(&self) -> Option<u32> { self.id() }
}

/// Why a gated spawn did not start a child.
#[derive(Debug)]
pub enum SpawnError {
    /// The host's link is down; no ssh was started.
    LinkDown,
    Io(std::io::Error),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::LinkDown => write!(f, "the host's link is down; no ssh was started"),
            SpawnError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SpawnError {}

/// One host's link state, shared by every frontend site that starts an ssh
/// login to it. While the link is down no gated spawn starts a child, so a
/// new start site cannot be written without asking the gate: the ungated
/// spawn functions above are private, and [`LinkGate::probe`] is the one
/// ungated spawn. Only the host's control transport writes the gate (up at
/// any hello reply, down when its session ends), and it is the only caller
/// of `probe`. A default gate is up. A close also ends every login in flight
/// that its start site watches through the gate ([`LinkGate::watch`]).
#[derive(Debug, Clone, Default)]
pub struct LinkGate {
    down: std::sync::Arc<std::sync::atomic::AtomicBool>,
    admission: std::sync::Arc<std::sync::Mutex<()>>,
    logins: std::sync::Arc<std::sync::Mutex<Logins>>,
    #[cfg(test)]
    rendezvous: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<tests::GateRendezvous>>>>,
}

impl LinkGate {
    pub fn is_up(&self) -> bool {
        !self.down.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Transport only.
    pub fn set_up(&self, up: bool) {
        #[cfg(test)]
        let observation = if up { None } else {
            self.rendezvous.lock().unwrap().clone().and_then(|hook| {
                let sender = hook.close.lock().unwrap().take()?;
                Some((hook, sender))
            })
        };
        #[cfg(test)]
        let _admission = self.observe_close_lock(observation.as_ref());
        #[cfg(not(test))]
        let _admission = self.admission.lock().unwrap_or_else(|error| error.into_inner());
        self.down.store(!up, std::sync::atomic::Ordering::Release);
        // Under the admission lock: a watch began before this close and is ended by it, or it finds the link down.
        if !up {
            for (_, end) in self.logins.lock().unwrap_or_else(|error| error.into_inner()).open.drain(..) {
                let _ = end.send(());
            }
        }
        #[cfg(test)]
        if let Some((hook, sender)) = observation {
            hook.events.lock().unwrap().push("closed".into());
            hook.snapshot.lock().unwrap().close = "completed";
            sender.send(tests::CloseObservation::Completed).unwrap();
        }
    }

    #[cfg(test)]
    fn observe_close_lock(&self, observation: Option<&(std::sync::Arc<tests::GateRendezvous>, std::sync::mpsc::Sender<tests::CloseObservation>)>) -> std::sync::MutexGuard<'_, ()> {
        if let Some((hook, sender)) = observation {
            match self.admission.try_lock() {
                Ok(guard) => return guard,
                Err(std::sync::TryLockError::Poisoned(error)) => return error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    hook.snapshot.lock().unwrap().close = "contended";
                    sender.send(tests::CloseObservation::Contended).unwrap();
                }
            }
        }
        self.admission.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn admit<C: StartedChild>(&self, command: std::process::Command, spawn: impl FnOnce(std::process::Command) -> std::io::Result<C>) -> Result<C, SpawnError> {
        let _admission = self.admission.lock().unwrap_or_else(|error| error.into_inner());
        if !self.is_up() { return Err(SpawnError::LinkDown); }
        #[cfg(test)]
        let rendezvous = self.rendezvous.lock().unwrap().clone();
        #[cfg(test)]
        let command = if let Some(hook) = &rendezvous { hook.prepare(command) } else { command };
        #[cfg(test)]
        if let Some(hook) = &rendezvous { hook.phase("os-spawn"); }
        #[cfg(not(test))]
        let child = spawn(command).map_err(SpawnError::Io)?;
        #[cfg(test)]
        let child = {
            let started = spawn(command);
            if let Some(hook) = &rendezvous { hook.phase("spawn-return"); }
            started.map_err(SpawnError::Io)?
        };
        #[cfg(test)]
        if let Some(hook) = &rendezvous { hook.created(child.child_id()); }
        #[cfg(test)]
        if let Some(hook) = &rendezvous { hook.phase("admission-return"); }
        Ok(child)
    }

    pub fn spawn_sync(&self, recipe: &SshRecipe) -> Result<std::process::Child, SpawnError> {
        self.admit(command(recipe), spawn_sync)
    }

    pub fn spawn_async(&self, recipe: &SshRecipe) -> Result<tokio::process::Child, SpawnError> {
        self.admit(command(recipe), spawn_async)
    }

    /// The gated command, not yet started, for a caller that contains the
    /// child it starts (the daemon, which kills the child's whole tree).
    pub fn command(&self, recipe: &SshRecipe) -> Result<std::process::Command, SpawnError> {
        if !self.is_up() {
            return Err(SpawnError::LinkDown);
        }
        Ok(command(recipe))
    }

    /// The one ungated spawn: the transport's own reconnect probe, which
    /// is what discovers that a link is back.
    pub fn probe(recipe: &SshRecipe) -> std::io::Result<tokio::process::Child> {
        spawn_async(command(recipe))
    }

    /// Watch a login through this gate from before it starts until its reply: a close in that time ends it
    /// ([`LinkWatch`]). `None` while the link is down, when no login starts.
    pub fn watch(&self) -> Option<LinkWatch> {
        let _admission = self.admission.lock().unwrap_or_else(|error| error.into_inner());
        if !self.is_up() {
            return None;
        }
        let (end, closed) = tokio::sync::oneshot::channel();
        let mut logins = self.logins.lock().unwrap_or_else(|error| error.into_inner());
        let id = logins.next;
        logins.next += 1;
        logins.open.push((id, end));
        Some(LinkWatch { listed: Listed { id, logins: std::sync::Arc::clone(&self.logins) }, closed })
    }
}

/// The logins in flight that a close of their gate ends, each by the sender its watch waits on.
#[derive(Debug, Default)]
struct Logins {
    next: u64,
    open: Vec<(u64, tokio::sync::oneshot::Sender<()>)>,
}

/// A login its gate watches ([`LinkGate::watch`]). A close of the gate before the watch drops ends it: the waiter in
/// [`LinkWatch::run`] or [`LinkWatch::closed`] wakes, and [`LinkWatch::ended`] is true from then on. Unwatched, a login
/// started before the link went down would wait out its own bound (`SSH_LOGIN_BOUND`) after the link is back. Dropping
/// the watch stops it.
#[derive(Debug)]
pub struct LinkWatch {
    listed: Listed,
    closed: tokio::sync::oneshot::Receiver<()>,
}

/// A watch's place among its gate's logins, given up when the watch stops.
#[derive(Debug)]
struct Listed {
    id: u64,
    logins: std::sync::Arc<std::sync::Mutex<Logins>>,
}

impl Drop for Listed {
    fn drop(&mut self) {
        self.logins.lock().unwrap_or_else(|error| error.into_inner()).open.retain(|(id, _)| *id != self.id);
    }
}

impl LinkWatch {
    /// Whether the gate has closed since the watch began.
    pub(crate) fn ended(&mut self) -> bool {
        !matches!(self.closed.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty))
    }

    /// Returns when the gate closes, for an async login; never, while the gate stays up.
    pub async fn closed(&mut self) {
        let _ = (&mut self.closed).await;
    }

    /// Run `login`, a blocking login on this thread, while a second thread waits for the gate's close and, if the close
    /// comes before `login` returns, calls `end`, the login's own cancel. Returns `login`'s result and whether the close
    /// ended it. With no thread to wait on the close, `login` runs unwatched, within its own bound.
    pub(crate) fn run<T>(self, end: impl Fn() + Sync, login: impl FnOnce() -> T) -> (T, bool) {
        let LinkWatch { listed, closed } = self;
        std::thread::scope(|scope| {
            let watcher = std::thread::Builder::new().name("sot-link-watch".to_string()).spawn_scoped(scope, || {
                let ended = closed.blocking_recv().is_ok();
                if ended {
                    end();
                }
                ended
            });
            let result = login();
            // The watch stops: its sender drops unless the close already took it, so the watcher returns either way.
            drop(listed);
            let ended = watcher.is_ok_and(|watcher| watcher.join().expect("the link watcher returns"));
            (result, ended)
        })
    }
}

/// After an async caller's own operation over a spawned child has
/// already failed, give a stderr drainer task a short bounded window to
/// finish landing the child's last line before giving up — stdout and
/// stderr are separate pipes with no ordering guarantee between them, so
/// a child that writes a diagnosis to stderr and closes stdout in the
/// same instant can otherwise be observed here before its line lands.
/// Async twin of `lane_client::BridgedClient`'s own (sync, blocking)
/// version of this same wait.
pub async fn last_stderr_after_failure(
    last_stderr: &std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
    loop {
        if let Some(line) = last_stderr.lock().ok().and_then(|g| g.clone()) {
            return Some(line);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

#[cfg(test)]
#[path = "ssh_bridge_tests.rs"]
mod tests;
