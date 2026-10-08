//! The pane_timing parent's private daemon, a second test-daemon starter beside the backend's Env: owned roots, rows, sentinels and owned teardown.

use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use sot_protocol::ops::{op, HelloReq};
use sot_protocol::{Frame, Kind};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const SELF_HOST: &str = "t3daemon";

/// One request/reply control connection to the private daemon, over a child that bridges stdio to it.
pub(super) struct Control {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Control {
    pub(super) fn from_child(mut child: Child) -> Result<Self> {
        let stdin = child.stdin.take().context("control child has no stdin")?;
        let out = BufReader::new(child.stdout.take().context("control child has no stdout")?);
        Ok(Self { child, stdin, out, next: 1 })
    }

    /// Says hello and returns the daemon's session id.
    pub(super) fn hello(&mut self, client: &str) -> Result<String> {
        let req = HelloReq::this_process(client, "", Some("t3parent".to_string()))?;
        let reply = self.call(op::HELLO, serde_json::to_value(req)?)?;
        reply["session_id"].as_str().map(str::to_string).context("hello carries no session_id")
    }

    pub(super) fn call(&mut self, name: &str, payload: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        sot_protocol::codec::write_frame_blocking(&mut self.stdin, &Frame::req(id, name, payload))?;
        loop {
            let frame = sot_protocol::codec::read_frame_blocking(&mut self.out)?;
            if frame.kind == Kind::Res && frame.id == id {
                ensure!(frame.payload.get("error").is_none(), "{name} failed: {}", frame.payload);
                return Ok(frame.payload);
            }
        }
    }

    pub(super) fn close(mut self) {
        drop(self.stdin);
        let _ = reap(&mut self.child, Duration::from_secs(10));
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

impl Daemon {
    /// Makes the roots, seeds `seeded` absent rows, starts the daemon and says hello to it.
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
        let mut daemon = Daemon { root: root.clone(), endpoint: endpoint.clone(), sotd: sotd.to_path_buf(), child: None, control: None, session_id: String::new(), rows: Vec::new() };
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
        for (name, _) in std::env::vars_os().filter(|(n, _)| n.to_string_lossy().starts_with("SOT_")) {
            command.env_remove(name);
        }
        command
            .arg("--socket").arg(&endpoint).arg("--project-root").arg(&anchor)
            .env("LOCALAPPDATA", &state).env("XDG_STATE_HOME", &state).env("XDG_CONFIG_HOME", &config)
            .env("SOT_SELF_HOST", SELF_HOST).env("SOT_RUNTIME_DIR", &runtime)
            .env("HOME", &home).env("USERPROFILE", &home).env("SOT_COMM_HOME", &comm)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        daemon.child = Some(command.spawn().context("start the private daemon")?);
        let (control, session) = daemon.open_control()?;
        daemon.control = Some(control);
        daemon.session_id = session;
        Ok(daemon)
    }

    /// A control connection to the daemon through the stand-in bridge, retried while the daemon binds; with its session id.
    fn open_control(&self) -> Result<(Control, String)> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let mut bridge = Command::new(&self.sotd);
            bridge.arg("stdio-bridge").arg("--endpoint").arg(format!("{}{}", if cfg!(windows) { "pipe:" } else { "unix:" }, self.endpoint.display()));
            bridge.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
            let mut control = Control::from_child(bridge.spawn()?)?;
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

    /// Destroys every row (each reply must report removal), kills and reaps the daemon, removes the roots.
    pub(super) fn teardown(mut self) -> Result<usize> {
        let mut problems = Vec::new();
        let mut destroyed = 0;
        let rows = self.rows.clone();
        for row in rows {
            match self.control().and_then(|c| c.call(op::WORKSPACE_DESTROY, json!({"workspace_id": row.id}))) {
                Ok(reply) if reply["kept"].is_null() && reply["toml_removed"] == true => destroyed += 1,
                Ok(reply) => problems.push(format!("destroy of {} reported {reply}", row.slug)),
                Err(e) => problems.push(format!("destroy of {} failed: {e}", row.slug)),
            }
        }
        if let Some(control) = self.control.take() {
            control.close();
        }
        if let Some(mut child) = self.child.take() {
            if !reap(&mut child, Duration::from_secs(10)) {
                problems.push("the private daemon was not reaped in its bound".to_string());
            }
        }
        for dir in [self.endpoint.parent().filter(|_| cfg!(unix)).map(Path::to_path_buf), Some(self.root.clone())].into_iter().flatten() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                problems.push(format!("remove {}: {e}", dir.display()));
            }
        }
        if !problems.is_empty() {
            bail!("pane teardown not confirmed: {}", problems.join("; "));
        }
        Ok(destroyed)
    }
}
