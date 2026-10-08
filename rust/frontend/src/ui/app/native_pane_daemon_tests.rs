//! The pane_timing parent's private daemon, a second test-daemon starter beside the backend's Env: owned roots, rows, sentinels and owned teardown.

use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use sot_protocol::ops::{op, HelloReq};
use sot_protocol::{Frame, Kind};
use sot_protocol::ops::lease::{LeaveIntent, CLOSE_ACK_WAIT};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SELF_HOST: &str = "t3daemon";

/// One request/reply control connection to the private daemon, over a child that bridges stdio to it. A reader
/// thread owns the child's stdout, so a call waits for its reply for at most `bound`.
pub(super) struct Control {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<anyhow::Result<Frame>>,
    bound: Duration,
    dead: Option<String>,
    next: u64,
}

impl Control {
    pub(super) fn from_child(mut child: Child, bound: Duration) -> Result<Self> {
        let stdin = child.stdin.take().context("control child has no stdin")?;
        let mut out = BufReader::new(child.stdout.take().context("control child has no stdout")?);
        let (tx, replies) = std::sync::mpsc::channel();
        std::thread::spawn(move || loop {
            let frame = sot_protocol::codec::read_frame_blocking(&mut out);
            let failed = frame.is_err();
            if tx.send(frame).is_err() || failed {
                break;
            }
        });
        Ok(Self { child, stdin: Some(stdin), replies, bound, dead: None, next: 1 })
    }

    /// Says hello and returns the daemon's session id.
    pub(super) fn hello(&mut self, client: &str) -> Result<String> {
        let req = HelloReq::this_process(client, "", Some("t3parent".to_string()))?;
        let reply = self.call(op::HELLO, serde_json::to_value(req)?)?;
        reply["session_id"].as_str().map(str::to_string).context("hello carries no session_id")
    }

    pub(super) fn call(&mut self, name: &str, payload: Value) -> Result<Value> {
        if let Some(why) = &self.dead {
            bail!("control is dead after an earlier failure: {why}");
        }
        let id = self.next;
        self.next += 1;
        let stdin = self.stdin.as_mut().context("control already closed")?;
        sot_protocol::codec::write_frame_blocking(stdin, &Frame::req(id, name, payload))?;
        let deadline = Instant::now() + self.bound;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(left) {
                Ok(Ok(frame)) if frame.kind == Kind::Res && frame.id == id => {
                    ensure!(frame.payload.get("error").is_none(), "{name} failed: {}", frame.payload);
                    return Ok(frame.payload);
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    self.dead = Some(e.to_string());
                    bail!("daemon request {name} lost its connection: {e}");
                }
                Err(RecvTimeoutError::Timeout) => {
                    let why = format!("daemon request {name} got no reply in {:?}", self.bound);
                    self.dead = Some(why.clone());
                    bail!("{why}");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.dead = Some("reader gone".into());
                    bail!("daemon request {name} lost its connection");
                }
            }
        }
    }

    /// Drops stdin and waits for the bridge's own exit; `wait_until` kills it only at the bound.
    pub(super) fn close(mut self) {
        drop(self.stdin.take());
        let _ = sot_log::test_isolated::wait_until(&mut self.child, Instant::now() + Duration::from_secs(10));
    }
}

/// Kills `child` and waits for it within `bound`; false when it did not end.
pub(super) fn reap(child: &mut Child, bound: Duration) -> bool {
    let _ = child.kill();
    let deadline = Instant::now() + bound;
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RowKind {
    Ready,
    Seeded,
}

#[derive(Clone, Debug)]
pub(super) struct Row {
    pub(super) id: String,
    pub(super) slug: String,
    pub(super) session: String,
    pub(super) kind: RowKind,
    pub(super) nonce: String,
}

pub(super) struct Daemon {
    root: PathBuf,
    pub(super) endpoint: PathBuf,
    sotd: PathBuf,
    child: Option<Child>,
    control: Option<Control>,
    pub(super) session_id: String,
    pub(super) rows: Vec<Row>,
    /// The one window lease this parent holds for the daemon's whole life, and the runtime that serves it.
    leases: Arc<crate::lease::Leases>,
    runtime: Option<tokio::runtime::Runtime>,
    roots: Vec<PathBuf>,
    done: bool,
}

pub(super) fn locate(name: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let path = exe.parent().and_then(Path::parent).context("test binary has no target folder")?.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    ensure!(path.exists(), "pane-timing prerequisite missing: build sotd and sot-capsule first");
    Ok(path)
}

fn stamp() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{:x}{:x}", std::process::id(), nanos & 0xffff_ffff_ffff)
}

const LEASE_HOST: &str = "t3lease";

impl Daemon {
    /// Makes the roots, seeds `seeded` absent rows, starts the daemon, says hello and takes the parent's lease.
    pub(super) fn start(sotd: &Path, seeded: usize) -> Result<Daemon> {
        let root = std::env::temp_dir().join(format!("sot-t3-{}", stamp()));
        let (state, config, home, comm, projects) = (root.join("state"), root.join("config"), root.join("home"), root.join("comm"), root.join("projects"));
        for dir in [&state, &config, &home, &comm, &projects] {
            std::fs::create_dir_all(dir)?;
        }
        #[cfg(unix)]
        let runtime = {
            use std::os::unix::fs::PermissionsExt;
            let dir = PathBuf::from(format!("/tmp/sot-t3rt-{}", stamp()));
            std::fs::create_dir(&dir)?;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            dir
        };
        #[cfg(not(unix))]
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&runtime)?;
        #[cfg(unix)]
        let endpoint = runtime.join("t3.sock");
        #[cfg(windows)]
        let endpoint = PathBuf::from(format!("\\\\.\\pipe\\sot-t3-{}", std::process::id()));
        let config_dir = if cfg!(windows) { state.join("sot").join("config") } else { config.join("sot") };
        let leases = crate::lease::Leases::new(false, vec![LEASE_HOST.to_string()]);
        let lease_rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build()?;
        let mut daemon = Daemon {
            root: root.clone(), endpoint: endpoint.clone(), sotd: sotd.to_path_buf(), child: None, control: None, session_id: String::new(), rows: Vec::new(),
            leases, runtime: Some(lease_rt), roots: vec![root.clone(), runtime.clone()], done: false,
        };
        let seeded_dir = config_dir.join(format!("workspaces-{SELF_HOST}"));
        std::fs::create_dir_all(&seeded_dir)?;
        for n in 0..seeded {
            let (id, slug, project) = (format!("t3seed{n}-{}", stamp()), format!("t3seed{n}"), projects.join(format!("seed{n}")));
            std::fs::create_dir_all(&project)?;
            let body = format!("workspace_id  = \"{id}\"\nslug          = \"{slug}\"\nproject_root  = \"{}\"\nruntime       = \"capsule\"\nagent         = \"none\"\n", project.display());
            std::fs::write(seeded_dir.join(format!("{slug}.toml")), body)?;
            daemon.rows.push(Row { id, slug, session: String::new(), kind: RowKind::Seeded, nonce: String::new() });
        }
        let anchor = projects.join("anchor");
        std::fs::create_dir_all(&anchor)?;
        let mut command = Command::new(sotd);
        for (name, _) in std::env::vars_os().filter(|(n, _)| n.to_string_lossy().to_uppercase().starts_with("SOT_")) {
            command.env_remove(name);
        }
        command
            .arg("--socket").arg(&endpoint).arg("--project-root").arg(&anchor)
            .env("LOCALAPPDATA", &state).env("XDG_STATE_HOME", &state).env("XDG_CONFIG_HOME", &config)
            .env("SOT_SELF_HOST", SELF_HOST).env("SOT_RUNTIME_DIR", &runtime)
            .env("HOME", &home).env("USERPROFILE", &home).env("SOT_COMM_HOME", &comm)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        daemon.child = Some(command.spawn().context("start the private daemon")?);
        let pid = daemon.child.as_ref().map_or(0, Child::id);
        println!("pane-timing daemon pid={pid} endpoint={} state={} roots={},{}", endpoint.display(), state.display(), root.display(), runtime.display());
        let (control, session) = daemon.open_control()?;
        daemon.control = Some(control);
        daemon.session_id = session;
        daemon.check_inherited_env(pid)?;
        daemon.take_lease()?;
        daemon.check_control_deadline(&runtime)?;
        Ok(daemon)
    }

    /// The parent's window lease, taken with the window's own client before any row exists: a parent that dies without a
    /// Close ends the lease connection silently, which the daemon treats as a Close (every row ended, exit 0).
    fn take_lease(&mut self) -> Result<()> {
        let rt = self.runtime.as_ref().context("lease runtime gone")?;
        let host = LEASE_HOST.to_string();
        let granted = rt.block_on(self.leases.before_data_connection(&host, &self.endpoint, None));
        if !matches!(granted, Ok(0)) || !self.leases.held(&host) {
            bail!("pane-timing could not lease its private daemon: {granted:?}");
        }
        Ok(())
    }

    /// O2: the daemon inherited no `SOT_` variable of this process beyond the three it is given (Linux).
    fn check_inherited_env(&self, pid: u32) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let environ = std::fs::read(format!("/proc/{pid}/environ")).context("read the daemon's environment")?;
            let allowed = ["SOT_SELF_HOST", "SOT_RUNTIME_DIR", "SOT_COMM_HOME"];
            for entry in environ.split(|b| *b == 0) {
                let name = String::from_utf8_lossy(entry.split(|b| *b == b'=').next().unwrap_or_default()).into_owned();
                ensure!(!name.to_uppercase().starts_with("SOT_") || allowed.contains(&name.as_str()), "the private daemon inherited {name}");
            }
        }
        let _ = pid;
        Ok(())
    }

    /// N7: a control whose peer never answers fails with its own timeout, within a bound (Unix).
    fn check_control_deadline(&self, runtime: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            let path = runtime.join("silent.sock");
            let _silent = std::os::unix::net::UnixListener::bind(&path)?;
            let sotd = self.sotd.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let outcome = (|| -> Result<String> {
                    let mut bridge = Command::new(&sotd);
                    bridge.arg("stdio-bridge").arg("--endpoint").arg(format!("unix:{}", path.display()));
                    bridge.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
                    let mut control = Control::from_child(bridge.spawn()?, Duration::from_secs(2))?;
                    let result = control.hello("t3-silent");
                    control.close();
                    Ok(result.err().map(|e| e.to_string()).unwrap_or_default())
                })();
                let _ = tx.send(outcome);
            });
            let started = Instant::now();
            let message = rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow::anyhow!("control call did not time out"))??;
            ensure!(message.contains("got no reply in") && started.elapsed() < Duration::from_secs(4), "control call did not time out: {message:?} after {:?}", started.elapsed());
        }
        let _ = runtime;
        Ok(())
    }

    /// A control connection to the daemon through the stand-in bridge, retried while the daemon binds; with its session id.
    fn open_control(&self) -> Result<(Control, String)> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let mut bridge = Command::new(&self.sotd);
            bridge.arg("stdio-bridge").arg("--endpoint").arg(format!("{}{}", if cfg!(windows) { "pipe:" } else { "unix:" }, self.endpoint.display()));
            bridge.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
            let mut control = Control::from_child(bridge.spawn()?, Duration::from_secs(30))?;
            match control.hello("t3-parent") {
                Ok(session) => return Ok((control, session)),
                Err(e) => {
                    control.close();
                    ensure!(Instant::now() < deadline, "the private daemon never answered: {e}");
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    fn control(&mut self) -> Result<&mut Control> {
        self.control.as_mut().context("no control connection")
    }

    /// The daemon's own root, where the parent also keeps the scenario file.
    pub(super) fn endpoint_root(&self) -> &Path {
        &self.root
    }

    pub(super) fn ready_rows(&self) -> Vec<Row> {
        self.rows.iter().filter(|r| r.kind == RowKind::Ready).cloned().collect()
    }

    pub(super) fn seeded_rows(&self) -> Vec<Row> {
        self.rows.iter().filter(|r| r.kind == RowKind::Seeded).cloned().collect()
    }

    /// Creates `count` Ready bare-shell rows, plants a nonce on each screen and fills in the seeded rows' session names.
    pub(super) fn make_ready_rows(&mut self, count: usize) -> Result<()> {
        for n in 0..count {
            let project = self.root.join("projects").join(format!("ready{n}"));
            std::fs::create_dir_all(&project)?;
            let reply = self.control()?.call(op::WORKSPACE_CREATE, json!({"label": format!("t3ready{n}"), "project_root": project.to_string_lossy(), "runtime": "capsule", "agent": "none"}))?;
            let id = reply["workspace_id"].as_str().context("no workspace_id")?.to_string();
            let session = reply["session_name"].as_str().context("no session_name")?.to_string();
            let nonce = format!("t3n{:08x}{n}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos()) ^ std::process::id());
            self.rows.push(Row { id: id.clone(), slug: String::new(), session, kind: RowKind::Ready, nonce });
            self.wait_phase(&id, "ready", Duration::from_secs(90))?;
        }
        let listed = self.list()?;
        for row in &mut self.rows {
            let entry = listed.iter().find(|e| e["workspace_id"] == row.id.as_str()).with_context(|| format!("row {} not listed", row.id))?;
            row.slug = entry["slug"].as_str().unwrap_or_default().to_string();
            row.session = entry["session_name"].as_str().unwrap_or_default().to_string();
        }
        for row in self.ready_rows() {
            self.plant(&row)?;
        }
        println!("pane-timing setup rows_ready={count} rows_seeded={} sentinels={count}", self.seeded_rows().len());
        Ok(())
    }

    fn plant(&mut self, row: &Row) -> Result<()> {
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(format!("echo {}", row.nonce));
        self.control()?.call(op::PTY_INPUT, json!({"workspace_id": row.id, "data_b64": data_b64, "enter": true}))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let screen = self.control()?.call(op::PTY_SCREEN, json!({"workspace_id": row.id}))?;
            let lines = screen["lines"].as_array().map(|l| l.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
            if lines.lines().any(|l| l.trim() == row.nonce) {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "sentinel never reached row {}'s screen: {}", row.slug, lines.trim_end());
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub(super) fn list(&mut self) -> Result<Vec<Value>> {
        let reply = self.control()?.call(op::WORKSPACE_LIST, json!({}))?;
        Ok(reply["workspaces"].as_array().cloned().unwrap_or_default())
    }

    fn phase_of(&mut self, id: &str) -> Result<Option<String>> {
        Ok(self.list()?.iter().find(|e| e["workspace_id"] == id).and_then(|e| e["phase"].as_str().map(str::to_string)))
    }

    fn wait_phase(&mut self, id: &str, want: &str, bound: Duration) -> Result<()> {
        let deadline = Instant::now() + bound;
        loop {
            let phase = self.phase_of(id)?;
            ensure!(phase.as_deref() != Some("terminal"), "row {id} went terminal");
            if phase.as_deref() == Some(want) {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "row {id} never reached {want} (last {phase:?})");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Before a timing run: every Ready row is `ready` and that run's seeded row is neither `starting` nor `ready`.
    pub(super) fn check_phases(&mut self, seeded: &Row) -> Result<()> {
        for row in self.ready_rows() {
            ensure!(self.phase_of(&row.id)?.as_deref() == Some("ready"), "Ready row {} is not ready", row.slug);
        }
        let phase = self.phase_of(&seeded.id)?;
        ensure!(!matches!(phase.as_deref(), Some("starting" | "ready")), "seeded row {} is already {phase:?}", seeded.slug);
        Ok(())
    }

    /// The one Close: the control connection ends, the lease leaves with `Close`, and the daemon must report zero not
    /// ended and exit 0 by itself; then the roots are removed. Returns how many roots were removed.
    pub(super) fn teardown(mut self) -> Result<usize> {
        self.done = true;
        let mut problems = Vec::new();
        if let Some(control) = self.control.take() {
            control.close();
        }
        match self.leases.leave_all(LeaveIntent::Close, 0, Instant::now()) {
            None => problems.push("the lease was not held at the Close".to_string()),
            Some(mut leaving) => loop {
                match leaving.poll(Instant::now()) {
                    crate::lease::LeaveStep::Exit => break,
                    crate::lease::LeaveStep::Wait(until) => std::thread::sleep(until.saturating_duration_since(Instant::now()).min(Duration::from_millis(100))),
                    crate::lease::LeaveStep::Show => {
                        problems.push(format!("the Close left sessions not ended: {}", leaving.line().unwrap_or_default()));
                        break;
                    }
                }
            },
        }
        problems.extend(self.await_daemon_exit(CLOSE_ACK_WAIT));
        Ok(self.remove_roots(&mut problems)).and_then(|n| {
            ensure!(problems.is_empty(), "pane teardown not confirmed: {}", problems.join("; "));
            Ok(n)
        })
    }

    /// Waits for the daemon's own exit within `bound`; it must be code 0. Nothing is signalled before the bound.
    fn await_daemon_exit(&mut self, bound: Duration) -> Vec<String> {
        let Some(mut child) = self.child.take() else { return Vec::new() };
        match sot_log::test_isolated::wait_until(&mut child, Instant::now() + bound) {
            Ok(status) if status.code() == Some(0) => Vec::new(),
            Ok(status) => vec![format!("the private daemon exited {status}, wanted 0")],
            Err(e) => vec![format!("the private daemon did not exit by itself: {e}")],
        }
    }

    fn remove_roots(&mut self, problems: &mut Vec<String>) -> usize {
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
        let mut removed = 0;
        for dir in std::mem::take(&mut self.roots) {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => removed += 1,
                // A root that is already gone counts as removed (on Windows the runtime folder lies inside the first root).
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => removed += 1,
                Err(e) => problems.push(format!("remove {}: {e}", dir.display())),
            }
        }
        removed
    }
}

impl Drop for Daemon {
    /// A parent that never ran its Close: ending the lease connection silently is a Close at the daemon.
    fn drop(&mut self) {
        if self.done {
            return;
        }
        drop(self.control.take());
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
        let mut problems = self.await_daemon_exit(Duration::from_secs(60));
        let removed = self.remove_roots(&mut problems);
        println!("pane-timing teardown after failure roots_removed={removed} problems={problems:?}");
    }
}

/// The kill proof (Linux): a parent killed mid-run leaves no process, row or root behind. The outer process starts the
/// stand-in parent, snapshots the parent's process tree while a child runs, kills only the parent pid its own spawn
/// returned, and then observes (never ends) the rest.
#[cfg(target_os = "linux")]
pub(super) fn kill_proof() -> Result<()> {
    use std::io::BufRead;
    let mut parent = Command::new(std::env::current_exe()?).arg("stand-in").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let ppid = parent.id();
    let (tx, lines) = std::sync::mpsc::channel();
    let stdout = parent.stdout.take().context("no stdout")?;
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let (mut daemon_line, mut roots, mut child_pid) = (String::new(), Vec::<PathBuf>::new(), String::new());
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let line = match lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => line,
            Err(_) => {
                let _ = parent.kill();
                let _ = parent.wait();
                bail!("kill-proof: the parent never started its first child");
            }
        };
        if let Some(rest) = line.strip_prefix("pane-timing daemon ") {
            daemon_line = rest.to_string();
            roots.extend(daemon_line.split("roots=").nth(1).unwrap_or_default().split(',').map(PathBuf::from));
        } else if let Some(hub) = line.strip_prefix("pane-timing roots hub=") {
            roots.push(PathBuf::from(hub));
        } else if let Some(rest) = line.strip_prefix("pane-timing child run-1 started pid=") {
            child_pid = rest.trim().to_string();
            break;
        }
    }
    // Killed while the child is still silent (it has listed no rows yet): only its stdin watch can end it.
    let snapshot = process_tree(ppid);
    parent.kill()?;
    parent.wait()?;
    let word = |key: &str| daemon_line.split_whitespace().find_map(|w| w.strip_prefix(key).map(str::to_string)).unwrap_or_default();
    let (daemon_pid, endpoint, state) = (word("pid="), PathBuf::from(word("endpoint=")), PathBuf::from(word("state=")));
    let mut failures = Vec::new();
    let gone_by = Instant::now() + Duration::from_secs(60);
    let mut alive = snapshot.clone();
    while Instant::now() < gone_by {
        alive.retain(|(pid, start, _)| process_start(*pid).as_ref() == Some(start));
        if alive.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if alive.iter().any(|(pid, _, _)| pid.to_string() == daemon_pid) {
        failures.push("daemon outlived its parent".to_string());
        let _ = close_by_lease(&endpoint);
    }
    for (pid, _, comm) in &alive {
        failures.push(format!("fixture process {pid} ({comm}) outlived its killed parent"));
    }
    if let Ok(text) = std::fs::read_to_string(state.join("sot").join("held.json")) {
        let not_ended = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["not_ended"].as_u64()).unwrap_or(0);
        if not_ended != 0 {
            failures.push(format!("the daemon recorded {not_ended} rows it could not end"));
        }
    }
    let rows = state.parent().map(|r| r.join("config").join("sot").join(format!("workspaces-{SELF_HOST}"))).unwrap_or_default();
    let mut rows_left = 0;
    for entry in std::fs::read_dir(&rows).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != "anchor.toml" {
            rows_left += 1;
            failures.push(format!("row {name} survived its daemon's close"));
        }
    }
    let prefix = format!("sot-state-migration-test-{child_pid}-");
    for entry in std::fs::read_dir(std::env::temp_dir()).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !child_pid.is_empty() && name.starts_with(&prefix) {
            failures.push(format!("fixture root {name} of the killed child remains"));
        }
    }
    let mut removed = 0;
    for dir in &roots {
        match std::fs::remove_dir_all(dir) {
            Ok(()) if !dir.exists() => removed += 1,
            other => failures.push(format!("root {} was not removed: {other:?}", dir.display())),
        }
    }
    ensure!(failures.is_empty(), "kill-proof failed: {}", failures.join("; "));
    println!("pane-timing kill-proof processes_gone={} held_not_ended=0 rows_left={rows_left} roots_removed={removed} ok=true", snapshot.len());
    Ok(())
}

/// Every descendant of `pid` as (pid, start time, comm), read from /proc; observing only.
#[cfg(target_os = "linux")]
fn process_tree(pid: u32) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        for task in std::fs::read_dir(format!("/proc/{p}/task")).into_iter().flatten().flatten() {
            let children = std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
            for child in children.split_whitespace().filter_map(|c| c.parse::<u32>().ok()) {
                if let Some(start) = process_start(child) {
                    let comm = std::fs::read_to_string(format!("/proc/{child}/comm")).unwrap_or_default().trim().to_string();
                    out.push((child, start, comm));
                    stack.push(child);
                }
            }
        }
    }
    out
}

/// The process's start time (field 22 of its stat), or None when it is gone or a zombie.
#[cfg(target_os = "linux")]
fn process_start(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 2..];
    let mut fields = after.split_whitespace();
    if fields.next()? == "Z" {
        return None;
    }
    fields.nth(18).map(str::to_string)
}

/// Takes a lease on `endpoint` with the window's own client and Closes it: the product's end for a daemon that outlived
/// its parent.
#[cfg(target_os = "linux")]
fn close_by_lease(endpoint: &Path) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build()?;
    let leases = crate::lease::Leases::new(false, vec![LEASE_HOST.to_string()]);
    rt.block_on(leases.before_data_connection(&LEASE_HOST.to_string(), endpoint, None))?;
    if let Some(mut leaving) = leases.leave_all(LeaveIntent::Close, 0, Instant::now()) {
        while let crate::lease::LeaveStep::Wait(until) = leaving.poll(Instant::now()) {
            std::thread::sleep(until.saturating_duration_since(Instant::now()).min(Duration::from_millis(100)));
        }
    }
    rt.shutdown_timeout(Duration::from_secs(2));
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn kill_proof() -> Result<()> {
    bail!("pane-timing kill-proof is Linux-only")
}
