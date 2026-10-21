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

#[cfg(unix)]
#[test]
fn a_child_keeps_no_descriptor_its_start_caught_before_close_on_exec() {
    use std::os::fd::AsRawFd;
    let (_reader, writer) = std::io::pipe().expect("pipe");
    let fd = writer.as_raw_fd();
    // SAFETY: fcntl changes only this process's flags on a descriptor the test holds open.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0, "clearing close-on-exec failed");
    let script = format!("if test -e /dev/fd/{fd}; then echo held; else echo none; fi");
    let recipe = SshRecipe::new("hub", None).unwrap().with_test_command("sh", ["-c", script.as_str()]);
    let ran = sot_platform::test_wait::run_within(&mut command(&recipe), std::time::Duration::from_secs(10)).ok();
    assert_eq!(ran.stdout.trim(), "none", "the child kept a descriptor its start caught before close-on-exec");
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
        // At 60 s a hang fails here, naming the wait and the gate's state.
        let result = receiver.recv_timeout(std::time::Duration::from_secs(60));
        if let Err(error) = &result { self.watchdog(event, &format!("{error:?}")); }
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
                .with_test_ssh_spawner(std::sync::Arc::new(|mut command| command.spawn()))
                .with_test_handshake_bound(sot_log::lane::transport::CONNECT_BOUND);
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
        let (_, claimed) = entries.recv_timeout(std::time::Duration::from_secs(30)).expect("an admission entry within 30 s");
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
