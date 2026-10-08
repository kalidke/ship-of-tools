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
    #[cfg(any(test, feature = "test-handshake-bound"))]
    if let Some(fixture) = &recipe.test_command {
        let mut cmd = std::process::Command::new(&fixture.program);
        cmd.args(&fixture.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        return cmd;
    }
    let (program, args) = argv(recipe);
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
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
/// of `probe`. A default gate is up.
#[derive(Debug, Clone, Default)]
pub struct LinkGate {
    down: std::sync::Arc<std::sync::atomic::AtomicBool>,
    admission: std::sync::Arc<std::sync::Mutex<()>>,
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
mod tests {
    use super::*;

    #[test]
    fn a_fixture_recipe_runs_its_fixture_command() {
        let plain = SshRecipe::new("hub", Some("far")).unwrap();
        let fixture = plain.clone().with_test_command("/fixture/prog", ["a", "b"]);
        let cmd = command(&fixture);
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/fixture/prog"), "fixture recipe ran ssh");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), [std::ffi::OsStr::new("a"), std::ffi::OsStr::new("b")]);
        let (program, args) = argv(&plain);
        let ssh = command(&plain);
        assert_eq!(ssh.get_program(), std::ffi::OsStr::new(program));
        assert_eq!(ssh.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>(), args);
        assert_eq!(fixture.to_string(), "ssh:hub/far");
        assert_eq!(plain.to_string(), "ssh:hub/far");
    }

    #[test]
    fn a_down_gate_starts_no_fixture_command() {
        let dir = std::env::temp_dir().join(format!("sot-fixture-missing-{}", std::process::id()));
        let recipe = SshRecipe::new("hub", None).unwrap().with_test_command(dir.join("no-such-program"), ["x"]);
        let gate = LinkGate::default();
        gate.set_up(false);
        assert!(matches!(gate.spawn_sync(&recipe), Err(SpawnError::LinkDown)), "down gate started a fixture child");
        gate.set_up(true);
        match gate.spawn_sync(&recipe) {
            Err(SpawnError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("admission did not reach the fixture program: {other:?}"),
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum CloseObservation { Contended, Completed }

    #[derive(Clone, Copy, Debug)]
    pub(super) struct GateSnapshot {
        mode: &'static str,
        phase: &'static str,
        admission: usize,
        ready: usize,
        releases_sent: usize,
        releases_consumed: usize,
        starts: usize,
        pub(super) close: &'static str,
    }

    impl GateSnapshot {
        fn new(mode: &'static str) -> Self {
            Self { mode, phase: "done", admission: 0, ready: 0, releases_sent: 0, releases_consumed: 0, starts: 0, close: "unobserved" }
        }
    }

    #[derive(Debug)]
    pub(super) struct GateRendezvous {
        program: std::path::PathBuf,
        ready: std::sync::mpsc::Sender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        pub(super) events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        pub(super) close: std::sync::Mutex<Option<std::sync::mpsc::Sender<CloseObservation>>>,
        // One-use: the first admitted start claims the pause, later starts proceed.
        pause: std::sync::atomic::AtomicBool,
        // Each prepare entry reports its ordinal and whether it claimed the pause.
        entries: std::sync::Mutex<Option<std::sync::mpsc::Sender<(usize, bool)>>>,
        pub(super) snapshot: std::sync::Mutex<GateSnapshot>,
    }

    impl GateRendezvous {
        fn new(program: &std::path::Path, mode: &'static str, pause: bool, events: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> (std::sync::Arc<Self>, std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
            let (ready, ready_rx) = std::sync::mpsc::channel();
            let (release, release_rx) = std::sync::mpsc::channel();
            let hook = Self { program: program.to_path_buf(), ready, release: std::sync::Mutex::new(release_rx), events, close: std::sync::Mutex::new(None), pause: std::sync::atomic::AtomicBool::new(pause), entries: std::sync::Mutex::new(None), snapshot: std::sync::Mutex::new(GateSnapshot::new(mode)) };
            (std::sync::Arc::new(hook), ready_rx, release)
        }

        pub(super) fn phase(&self, phase: &'static str) { self.snapshot.lock().unwrap().phase = phase; }

        fn watchdog(&self, event: &str, result: &str) {
            let state = *self.snapshot.lock().unwrap();
            println!("T2 gate watchdog: mode={} wait={event} result={result} phase={} admission={} ready={} releases-sent={} releases-consumed={} starts={} close={}", state.mode, state.phase, state.admission, state.ready, state.releases_sent, state.releases_consumed, state.starts, state.close);
        }

        fn wait_entry(&self, event: &str) {
            let mode = self.snapshot.lock().unwrap().mode;
            println!("T2 gate wait: mode={mode} wait={event}");
        }

        fn wait<T>(&self, receiver: &std::sync::mpsc::Receiver<T>, event: &str, panic: &str) -> T {
            self.wait_entry(event);
            // A genuine hang is bounded by the hosted job's own guard; no local deadline claims progress.
            let result = receiver.recv();
            if result.is_err() { self.watchdog(event, "Disconnected"); }
            result.expect(panic)
        }

        fn release(&self, sender: &std::sync::mpsc::Sender<()>) {
            let mut state = self.snapshot.lock().unwrap();
            let result = sender.send(());
            if result.is_ok() { state.releases_sent += 1; }
            drop(state);
            result.unwrap();
        }

        pub(super) fn prepare(&self, prepared: std::process::Command) -> std::process::Command {
            assert!(prepared.get_args().any(|arg| arg == "ControlMaster=no"), "one prepared argv authority");
            let claimed = self.pause.swap(false, std::sync::atomic::Ordering::AcqRel);
            let ordinal = { let mut state = self.snapshot.lock().unwrap(); state.admission += 1; state.phase = "admission-release"; state.admission };
            if let Some(entries) = &*self.entries.lock().unwrap() { entries.send((ordinal, claimed)).unwrap(); }
            if claimed {
                { let mut state = self.snapshot.lock().unwrap(); self.ready.send(()).unwrap(); state.ready += 1; }
                self.wait(&self.release.lock().unwrap(), "admission-release", "admission rendezvous watchdog");
                self.snapshot.lock().unwrap().releases_consumed += 1;
            }
            let mut command = std::process::Command::new("python3");
            command.arg("-u").arg(&self.program).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
            command
        }

        pub(super) fn created(&self, id: Option<u32>) {
            self.events.lock().unwrap().push(format!("created {}", id.expect("a successful spawn owns a child id")));
            let mut state = self.snapshot.lock().unwrap();
            state.starts += 1; state.phase = "created";
        }
    }

    fn cleanup_sync(child: &mut std::process::Child, hook: &GateRendezvous) {
        hook.phase("cleanup"); hook.wait_entry("child-reaped");
        if child.try_wait().unwrap().is_none() { child.kill().expect("terminate owned gate child"); }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline { hook.watchdog("child-reaped", "Timeout"); }
            assert!(std::time::Instant::now() < deadline, "owned gate child reap watchdog");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn gate_start(mode: &str, gate: &LinkGate, voyage: bool) -> bool {
        let recipe = SshRecipe::new("teststub", None).unwrap();
        let hook = gate.rendezvous.lock().unwrap().clone().unwrap();
        let refused = match mode {
            "sync" => match gate.spawn_sync(&recipe) {
                Ok(mut child) => { cleanup_sync(&mut child, &hook); false }
                Err(SpawnError::LinkDown) => true,
                Err(error) => panic!("gate fixture failed: {error}"),
            },
            "async" => {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                let _entered = runtime.enter();
                match gate.spawn_async(&recipe) {
                    Ok(mut child) => {
                        hook.phase("cleanup"); hook.wait_entry("child-reaped");
                        child.start_kill().expect("terminate owned async gate child");
                        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                        while child.try_wait().unwrap().is_none() {
                            if std::time::Instant::now() >= deadline { hook.watchdog("child-reaped", "Timeout"); }
                            assert!(std::time::Instant::now() < deadline, "owned async gate child reap watchdog");
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        false
                    }
                    Err(SpawnError::LinkDown) => true,
                    Err(error) => panic!("gate fixture failed: {error}"),
                }
            }
            "fixture" => {
                use sot_log::lane::client::Endpoint;
                use super::super::lane_client::{DaemonLaneEndpoint, LaneDial};
                let endpoint = DaemonLaneEndpoint::new(LaneDial::Ssh(recipe, gate.clone()), None)
                    .with_test_ssh_spawner(std::sync::Arc::new(|mut command| command.spawn()));
                hook.phase("endpoint-handshake");
                let result = if voyage { endpoint.connect_voyage_unchallenged("fixture-row", "fixture-voyage") }
                    else { endpoint.connect_supervisor_unchallenged("fixture-row") };
                hook.phase("cleanup"); hook.wait_entry("endpoint-reaped");
                let refused = matches!(result, Err(sot_log::lane::transport::TransportError::LinkDown));
                drop(result); drop(endpoint);
                refused
            }
            _ => unreachable!(),
        };
        hook.phase("done");
        refused
    }

    fn gate_race(mode: &'static str, program: &std::path::Path) -> bool {
        use std::sync::{Arc, Mutex, mpsc};
        let gate = LinkGate::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let (hook, ready, release) = GateRendezvous::new(program, mode, true, events.clone());
        *gate.rendezvous.lock().unwrap() = Some(hook.clone());
        gate.set_up(false);
        assert!(gate_start(mode, &gate, true), "close-first must return LinkDown");
        assert!(events.lock().unwrap().is_empty(), "close-first creates no child");
        gate.set_up(true);
        let admitted = gate.clone();
        let (spawn_done_tx, spawn_done) = mpsc::channel();
        let starter = std::thread::spawn(move || { let result = gate_start(mode, &admitted, false); spawn_done_tx.send(result).unwrap(); });
        hook.wait(&ready, "admission-ready", "spawn reached its admission rendezvous");
        let closing = gate.clone();
        let (closed_tx, closed_rx) = mpsc::channel();
        // Arm only this actual close attempt, after admitted creation is held.
        *hook.close.lock().unwrap() = Some(closed_tx);
        let closer = std::thread::spawn(move || closing.set_up(false));
        let first = hook.wait(&closed_rx, "close-first", "owner close-lock observation watchdog");
        hook.release(&release);
        if first == CloseObservation::Contended {
            assert_eq!(hook.wait(&closed_rx, "close-completed", "owner down-transition watchdog"), CloseObservation::Completed);
        }
        hook.wait(&spawn_done, "starter-done", "owned child completion watchdog");
        starter.join().unwrap(); closer.join().unwrap();
        let observed = events.lock().unwrap().clone();
        let closed: Vec<_> = observed.iter().enumerate().filter(|(_, event)| *event == "closed").map(|(index, _)| index).collect();
        let starts = observed.iter().filter(|event| event.starts_with("created ")).count();
        let held_before_close = closed.len() == 1 && observed.first().is_some_and(|event| event.starts_with("created ")) && closed[0] > 0;
        let all_before_close = closed.len() == 1 && closed[0] == starts;
        let paused = hook.snapshot.lock().unwrap().releases_consumed;
        let count = observed.len();
        let refused = gate_start(mode, &gate, true);
        assert!(refused, "after close returns the gate must return LinkDown");
        assert_eq!(events.lock().unwrap().len(), count, "after close creates no additional child");
        let (reopened, _, _) = GateRendezvous::new(program, mode, false, events.clone());
        *gate.rendezvous.lock().unwrap() = Some(reopened);
        gate.set_up(true);
        let count = events.lock().unwrap().len();
        gate_start(mode, &gate, true);
        let reopen_starts = events.lock().unwrap().len() - count;
        println!("T2 gate order: mode={mode} first={first:?} held-start-before-close={held_before_close} all-starts-before-close={all_before_close} after-close-refused={refused} reopen-starts={reopen_starts} paused={paused}");
        assert_eq!(reopen_starts, 1, "reopen admits one ordinary child start");
        let start_bound = if mode == "fixture" { 2 } else { 1 };
        first == CloseObservation::Contended && held_before_close && all_before_close && (1..=start_bound).contains(&starts) && paused == 1
    }

    /// The contested pause is one-use: of two admissions through one armed hook, exactly one pauses.
    #[test]
    fn admission_rendezvous_pauses_once_per_armed_hook() {
        use std::sync::{Arc, Mutex, mpsc};
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("sot-gate-pause-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        std::fs::create_dir(&path).unwrap();
        let program = path.join("child.py");
        sot_log::test_exec::write_executable(&program, "import sys\nsys.stdin.buffer.read()\n");
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture { fn drop(&mut self) { std::fs::remove_dir_all(&self.0).expect("remove own gate fixture"); } }
        let _fixture = Fixture(path);
        let gate = LinkGate::default();
        let (hook, _ready, release) = GateRendezvous::new(&program, "pause", true, Arc::new(Mutex::new(Vec::new())));
        let (entries_tx, entries) = mpsc::channel();
        *hook.entries.lock().unwrap() = Some(entries_tx);
        *gate.rendezvous.lock().unwrap() = Some(hook.clone());
        let recipe = SshRecipe::new("teststub", None).unwrap();
        let starters: Vec<_> = (0..2).map(|_| { let gate = gate.clone(); let recipe = recipe.clone(); std::thread::spawn(move || gate.spawn_sync(&recipe).expect("admitted start")) }).collect();
        let mut pauses = 0;
        for _ in 0..2 {
            let (_, claimed) = entries.recv().expect("an admission entry");
            if claimed { pauses += 1; hook.release(&release); }
        }
        let mut reaped = true;
        for starter in starters {
            let mut child = starter.join().unwrap();
            cleanup_sync(&mut child, &hook);
            reaped &= child.try_wait().unwrap().is_some();
        }
        println!("T2 gate pause: admissions=2 pauses={pauses} owned-children-reaped={reaped}");
        assert_eq!(pauses, 1, "one armed admission rendezvous must pause exactly once");
    }

    #[test]
    fn spawn_admission_and_link_close_are_one_decision() {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("sot-gate-fixture-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        std::fs::create_dir(&path).unwrap();
        let program = path.join("child.py");
        sot_log::test_exec::write_executable(&program, "import sys\nsys.stdin.buffer.read()\n");
        // Always remove the test-owned absolute folder, including on a reversal assertion.
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture { fn drop(&mut self) { std::fs::remove_dir_all(&self.0).expect("remove own gate fixture"); } }
        let _fixture = Fixture(path);
        let observations: Vec<_> = ["sync", "async", "fixture"].into_iter().map(|mode| (mode, gate_race(mode, &program))).collect();
        // All three modes have released creation, reaped their children and joined before this verdict.
        assert!(observations.iter().all(|(_, valid)| *valid), "a real child start must precede close completion: {observations:?}");
    }

    #[test]
    fn a_down_gate_spawns_no_child() {
        let recipe = SshRecipe::new("hub", None).unwrap();
        let gate = LinkGate::default();
        assert!(gate.is_up());
        gate.set_up(false);
        assert!(matches!(gate.spawn_sync(&recipe), Err(SpawnError::LinkDown)));
        assert!(matches!(gate.spawn_async(&recipe), Err(SpawnError::LinkDown)));
        assert!(matches!(gate.command(&recipe), Err(SpawnError::LinkDown)));
        assert!(matches!(gate.clone().spawn_sync(&recipe), Err(SpawnError::LinkDown)), "clones share one flag");
    }

    #[test]
    fn recipe_rejects_a_leading_dash_in_either_half() {
        assert!(SshRecipe::new("-oProxyCommand=x", None).is_err());
        assert!(SshRecipe::new("hub", Some("-oProxyCommand=x")).is_err());
    }

    #[test]
    fn recipe_rejects_grammar_outside_a_plain_host_name() {
        for bad in ["Hub", "hub;rm -rf", "hub name", "hub@host", ""] {
            assert!(SshRecipe::new(bad, None).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn argv_has_no_shell_and_the_stated_option_set() {
        let recipe = SshRecipe::new("hub", None).unwrap();
        let (program, args) = argv(&recipe);
        assert_eq!(program, "ssh");
        assert_eq!(
            args,
            vec![
                "-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", "-o", "ConnectTimeout=10", "-o", "ControlMaster=no", "-o", "ControlPath=none", "-o", "ControlPersist=no", "hub",
                "export PATH=\"$HOME/.local/share/sot/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH\"; sotd stdio-bridge",
            ]
        );
    }

    #[test]
    fn argv_appends_host_only_when_the_recipe_carries_one() {
        let recipe = SshRecipe::new("hub", Some("gamma")).unwrap();
        let (_, args) = argv(&recipe);
        assert!(args.last().unwrap().ends_with("sotd stdio-bridge --host gamma"));

        let no_host = SshRecipe::new("hub", None).unwrap();
        let (_, args) = argv(&no_host);
        assert!(args.last().unwrap().ends_with("sotd stdio-bridge"));
        assert!(!args.last().unwrap().contains("--host"));
    }

    #[test]
    fn display_matches_the_endpoint_grammar() {
        assert_eq!(SshRecipe::new("hub", None).unwrap().to_string(), "ssh:hub");
        assert_eq!(SshRecipe::new("hub", Some("gamma")).unwrap().to_string(), "ssh:hub/gamma");
    }
}
