//! The opt-in native relayed-attach timing fixture: the real App over an `ssh:<hub>/<host>` dial; the parent takes the route as its argument and runs each frontend as a child of its binary, whose control values are its arguments.

use super::native_pane_daemon_tests::{locate, Daemon, Row};
use super::native_pane_route_tests::{recipe_from, Route, RouteKind};
use super::tests::{fixture_cli, FixtureHome};
use super::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use winit::event_loop::{EventLoop, EventLoopProxy};

const HOST_KEY: &str = "t3host";
const RECEIPT: &str = "session pane: capsule screen presented";
const FAILURE: &str = "fixture: candidate frame failed before present";
/// Only a missing display or adapter is "not runnable".
const DISPLAY_MARKER: &str = "not runnable here: display:";
const READY_BOUND: Duration = Duration::from_secs(30);
const RESUME_BOUND: Duration = Duration::from_secs(60);
const CHILD_BOUND: Duration = Duration::from_secs(240);

static FAIL_NEXT_CANDIDATE_FRAME: AtomicBool = AtomicBool::new(false);

/// The hook `redraw` asks before it submits a frame that carries a presentation candidate; the fault child arms it.
pub(super) fn take_candidate_frame_failure() -> bool {
    FAIL_NEXT_CANDIDATE_FRAME.swap(false, Ordering::SeqCst)
}

pub(crate) fn run_native_pane_timing() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [c, role, path] if c == "child" => child(role, std::path::Path::new(path)),
        [route] if route == "ssh-relay" => parent(RouteKind::SshRelay),
        [route] if route == "stand-in" => parent(RouteKind::StandIn),
        [proof] if proof == "kill-proof" => super::native_pane_daemon_tests::kill_proof(),
        _ => anyhow::bail!("pane-timing needs one route argument: ssh-relay or stand-in"),
    }
}

// ---- parent ----

struct ChildRun {
    role: String,
    ok: bool,
    out: String,
    err: String,
}

fn parent(kind: RouteKind) -> Result<()> {
    if kind == RouteKind::SshRelay && !cfg!(unix) {
        anyhow::bail!("pane-timing ssh-relay route is Unix-only");
    }
    let sotd = locate("sotd")?;
    locate("sot-capsule")?;
    let mut daemon = Daemon::start(&sotd, 3)?;
    let mut route = None;
    let runs = run_children(kind, &sotd, &mut daemon, &mut route);
    let route_down = route.as_mut().map(Route::teardown).transpose();
    let down = daemon.teardown();
    let teardown = match (route_down, down) {
        (Ok(_), Ok(roots)) => {
            println!("pane-timing teardown close not_ended=0 daemon_exit=0 roots_removed={roots}");
            Ok(())
        }
        (route_down, down) => {
            let detail = [route_down.err().map(|e| e.to_string()), down.err().map(|e| e.to_string())].into_iter().flatten().collect::<Vec<_>>().join("; ");
            println!("pane-timing teardown not confirmed: {detail}");
            Err(anyhow::anyhow!("pane teardown not confirmed: {detail}"))
        }
    };
    // The children's own result first (a missing display reads as "not runnable"), then the teardown's.
    let judged = runs.and_then(|runs| verdict(kind, &runs));
    match (judged, teardown) {
        (Err(e), Err(t)) => Err(anyhow::anyhow!("{e:#}\n{t:#}")),
        (Err(e), Ok(())) => Err(e),
        (Ok(()), Err(t)) => Err(t),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn run_children(kind: RouteKind, sotd: &std::path::Path, daemon: &mut Daemon, route: &mut Option<Route>) -> Result<Vec<ChildRun>> {
    daemon.make_ready_rows(5)?;
    let started = route.insert(Route::start(kind, sotd, &daemon.endpoint)?);
    super::native_pane_route_tests::preflight(started, daemon)?;
    let (program, args) = started.program_and_args();
    let ready = daemon.ready_rows();
    let seeded = daemon.seeded_rows();
    let row = |r: &Row| json!({"slug": r.slug, "nonce": r.nonce, "session": r.session});
    let scenario = json!({
        "route": kind.name(), "host_key": HOST_KEY, "host": started.host, "program": program.to_string_lossy(), "args": args,
        "ready": ready.iter().map(row).collect::<Vec<_>>(), "seeded": seeded.iter().map(row).collect::<Vec<_>>(),
    });
    let path = daemon_scenario_path(daemon);
    std::fs::write(&path, serde_json::to_vec_pretty(&scenario)?)?;
    let mut runs = Vec::new();
    for (n, role) in ["run-1", "run-2", "run-3", "fault"].into_iter().enumerate() {
        if role != "fault" {
            daemon.check_phases(&seeded[n])?;
        }
        let before = route.as_ref().and_then(Route::relay_accepts);
        let run = run_child(role, &path)?;
        if let (Some(before), Some(after)) = (before, route.as_ref().and_then(Route::relay_accepts)) {
            let attaches = if role == "fault" { 1 } else { 6 };
            anyhow::ensure!(after >= before + 1 + 2 * attaches, "relayed run used no relay hop: {role} grew {} accepts, wanted {}", after - before, 1 + 2 * attaches);
        }
        let unrunnable = run.out.contains(DISPLAY_MARKER) || run.err.contains(DISPLAY_MARKER);
        runs.push(run);
        if unrunnable {
            break;
        }
    }
    Ok(runs)
}

fn daemon_scenario_path(daemon: &Daemon) -> std::path::PathBuf {
    daemon.endpoint_root().join("scenario.json")
}

/// Reads `pipe` to its end on a thread of its own.
fn read_all(pipe: Option<impl std::io::Read + Send + 'static>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        let _ = tx.send(text);
    });
    rx
}

/// Runs one child with its stdin held open until it has exited, so a parent that dies takes the child with it.
fn run_child(role: &str, scenario: &std::path::Path) -> Result<ChildRun> {
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(["child", role])
        .arg(scenario)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    println!("pane-timing child {role} started pid={}", child.id());
    let _stdin = child.stdin.take();
    let (out_rx, err_rx) = (read_all(child.stdout.take()), read_all(child.stderr.take()));
    let waited = sot_log::test_isolated::wait_until(&mut child, Instant::now() + CHILD_BOUND);
    let out = out_rx.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    let mut err = err_rx.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    let ok = match &waited {
        Ok(status) => {
            println!("pane-timing child {role} status={status}");
            status.success()
        }
        Err(e) => {
            err.push_str(&format!("child {role} did not finish in its bound; cleanup: {e}\n"));
            false
        }
    };
    Ok(ChildRun { role: role.to_string(), ok, out, err })
}

fn field(line: &str, name: &str) -> Option<String> {
    line.split_whitespace().find_map(|w| w.strip_prefix(&format!("{name}=")).map(str::to_string))
}

fn verdict(kind: RouteKind, runs: &[ChildRun]) -> Result<()> {
    let route = kind.name();
    if let Some(r) = runs.iter().find(|r| r.out.contains(DISPLAY_MARKER) || r.err.contains(DISPLAY_MARKER)) {
        anyhow::bail!("pane-timing not runnable here: {}: {}{}", r.role, r.out, r.err);
    }
    let mut ready: Vec<(String, u128, u128)> = Vec::new();
    let mut resume = Vec::new();
    // The fault child first: its order assertion is the cause when a timing run's attribution also breaks.
    for run in runs.iter().filter(|r| r.role == "fault").chain(runs.iter().filter(|r| r.role != "fault")) {
        anyhow::ensure!(run.ok, "pane-timing child {} failed:\n{}{}", run.role, run.out, run.err);
        anyhow::ensure!(run.out.matches("pane-timing: body entered").count() == 1, "pane timing body did not enter exactly once: {}", run.role);
        if run.role == "fault" {
            let line = run.out.lines().find(|l| l.starts_with("pane-timing fault")).ok_or_else(|| anyhow::anyhow!("fault child printed no verdict: {}", run.out))?;
            anyhow::ensure!(line.contains("ok=true"), "fault child verdict not ok: {line}");
            println!("{line} route={route}");
            continue;
        }
        anyhow::ensure!(run.out.contains(&format!("pane-timing run={} completed", &run.role[4..])), "{} did not complete: {}", run.role, run.out);
        for line in run.out.lines().filter(|l| l.starts_with("pane-timing sample")) {
            let (kind, total) = (field(line, "kind").unwrap_or_default(), field(line, "total_ns").and_then(|v| v.parse::<u128>().ok()));
            if kind == "resume" {
                resume.push(line.to_string());
                anyhow::ensure!(field(line, "outcome").is_some_and(|o| o == "presented" || o.starts_with("failed")), "resume attach neither presented nor failed typed within 60 s: {line}");
                continue;
            }
            let total = total.ok_or_else(|| anyhow::anyhow!("ready sample without a receipt: {line}"))?;
            let checkpoint = field(line, "checkpoint_ms").and_then(|v| v.parse::<i128>().ok()).filter(|c| *c >= 0);
            anyhow::ensure!(checkpoint.is_some_and(|c| (c as u128) * 1_000_000 <= total), "receipt before its checkpoint: {line}");
            anyhow::ensure!(field(line, "sentinel").as_deref() == Some("true"), "pane sample without its row's sentinel: {line}");
            let driver = field(line, "driver_ns").and_then(|v| v.parse::<u128>().ok()).unwrap_or(0);
            anyhow::ensure!(driver >= total && driver - total <= 50_000_000, "receipt elapsed and driver elapsed disagree by {}ns: {line}", driver.abs_diff(total));
            ready.push((line.to_string(), total, driver));
        }
    }
    anyhow::ensure!(ready.len() == 15, "wanted 15 cold Ready samples, got {}", ready.len());
    for (line, ..) in &ready {
        println!("{line} route={route}");
    }
    for line in &resume {
        println!("{line} route={route}");
    }
    let mut totals: Vec<u128> = ready.iter().map(|r| r.1).collect();
    totals.sort_unstable();
    let bound = sot_log::lane::transport::CONNECT_BOUND.as_nanos();
    let ok = totals.iter().all(|t| *t <= bound);
    println!(
        "pane-timing route={route} ready_pane_total_fits_connect_bound samples=15 max_ms={} p50_ms={} ok={ok}",
        totals[14] / 1_000_000,
        totals[7] / 1_000_000
    );
    println!("pane-timing route={route} resume_pane_records_the_whole_operation samples={}", resume.len());
    anyhow::ensure!(ok, "pane presentation missed CONNECT_BOUND: {:?}", Duration::from_nanos(*totals.last().unwrap() as u64));
    Ok(())
}

// ---- child ----

/// The scenario file: every field required, so a missing one fails as malformed.
#[derive(serde::Deserialize)]
struct Scenario {
    host: String,
    program: String,
    args: Vec<String>,
    ready: Vec<RowSpec>,
    seeded: Vec<RowSpec>,
}

#[derive(serde::Deserialize)]
struct RowSpec {
    slug: String,
    nonce: String,
    session: String,
}

struct Plan {
    slug: String,
    nonce: String,
    session: String,
    resume: bool,
}

struct Current {
    at: Instant,
    mark: usize,
    seen: Option<Seen>,
}

struct Seen {
    total_ns: u128,
    driver_ns: u128,
    open_ms: Option<u128>,
    sentinel: bool,
}

struct PaneDriver<'a> {
    app: &'a mut App,
    capture: &'a sot_log::test_log::Capture,
    role: String,
    plan: Vec<Plan>,
    idx: usize,
    cur: Option<Current>,
    begun: Instant,
    targets: Vec<String>,
    stop: std::sync::Arc<AtomicBool>,
    finished: bool,
    failure: Option<anyhow::Error>,
}

fn receipts(text: &str) -> Vec<u128> {
    text.lines().filter(|l| l.contains(RECEIPT)).filter_map(|l| field(l, "since_request_ns").and_then(|v| v.parse().ok())).collect()
}

impl PaneDriver<'_> {
    fn cells(&self) -> String {
        let Some(state) = self.app.state.as_ref() else { return String::new() };
        state.terminal.backend().project_lines(0.0, 0.0, 1.0, 1.0).into_iter().map(|l| l.text).collect::<Vec<_>>().join("\n")
    }

    fn ready_to_switch(&self) -> Result<bool> {
        let Some(state) = self.app.state.as_ref() else { return Ok(false) };
        let listed = state.workspace_lists.get(HOST_KEY).is_some_and(|rows| self.plan.iter().all(|p| rows.iter().any(|r| r.slug == p.slug)));
        if !(listed && state.frame_counter >= 3 && state.pty_size.is_some()) {
            anyhow::ensure!(self.begun.elapsed() < Duration::from_secs(90), "pane-timing setup failure: the window never listed its rows in 90 s");
            return Ok(false);
        }
        let (cols, rows) = state.pty_size.unwrap_or((0, 0));
        anyhow::ensure!(cols > 0 && rows > 0, "pane-timing setup failure: the agent pane has no area");
        println!("pane-timing pane cols={cols} rows={rows}");
        Ok(true)
    }

    fn switch(&mut self) -> Result<()> {
        let session = self.plan[self.idx].session.clone();
        anyhow::ensure!(!self.targets.contains(&session), "row {} was already the pane target before its switch", self.plan[self.idx].slug);
        let mark = self.capture.text().len();
        if self.role == "fault" {
            FAIL_NEXT_CANDIDATE_FRAME.store(true, Ordering::SeqCst);
        }
        let slug = self.plan[self.idx].slug.clone();
        let at = Instant::now();
        if let Some(state) = self.app.state.as_mut() {
            state.switch_to_workspace(HOST_KEY.to_string(), Some(slug), None, true);
        }
        self.cur = Some(Current { at, mark, seen: None });
        Ok(())
    }

    fn step(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        self.note_target();
        if self.cur.is_none() {
            if self.ready_to_switch()? {
                self.switch()?;
            }
            return Ok(());
        }
        let text = self.capture.text();
        let (at, mark, seen_before) = {
            let c = self.cur.as_ref().unwrap();
            (c.at, c.mark, c.seen.is_some())
        };
        let new = &text[mark..];
        let plan = &self.plan[self.idx];
        let bound = if plan.resume { RESUME_BOUND } else { READY_BOUND };
        if !seen_before {
            if let Some(first) = receipts(new).first() {
                self.check_fault_order(new)?;
                let seen = Seen { total_ns: *first, driver_ns: at.elapsed().as_nanos(), open_ms: self.open_ms(), sentinel: self.cells().contains(&plan.nonce) };
                return self.finish_sample(event_loop, Some(seen), "presented".to_string(), new.to_string());
            } else if at.elapsed() > bound {
                anyhow::ensure!(self.role != "fault", "fault child saw no receipt within {bound:?}");
                return self.finish_sample(event_loop, None, "timeout".to_string(), new.to_string());
            } else if let Some(outcome) = self.dead_client_outcome(plan.resume) {
                return self.finish_sample(event_loop, None, outcome, new.to_string());
            }
        }
        Ok(())
    }

    /// Every pane target this child has observed, at every step.
    fn note_target(&mut self) {
        if let Some((_, session)) = self.app.state.as_ref().and_then(|s| s.bl_pane_target.clone()) {
            if !self.targets.contains(&session) {
                self.targets.push(session);
            }
        }
    }

    /// The pty.open reply installing the client, from the request.
    fn open_ms(&self) -> Option<u128> {
        let state = self.app.state.as_ref()?;
        Some(state.pane_attach_started_at?.saturating_duration_since(state.pane_attach_requested_at?).as_millis())
    }

    fn dead_client_outcome(&mut self, resume: bool) -> Option<String> {
        let t = self.app.state.as_mut()?.pane_attach_term.as_mut()?;
        (resume && t.is_dead()).then(|| format!("failed:{}", t.status_line().replace(' ', "_")))
    }

    /// The fault child's order guard: a receipt line before the failure line means selection completed the attach.
    fn check_fault_order(&self, new: &str) -> Result<()> {
        if self.role != "fault" {
            return Ok(());
        }
        let receipt = new.find(RECEIPT);
        match (new.find(FAILURE), receipt) {
            (Some(f), Some(r)) if r > f => Ok(()),
            (_, Some(_)) => anyhow::bail!("presentation completed before frame.present"),
            _ => Ok(()),
        }
    }

    fn finish_sample(&mut self, event_loop: &ActiveEventLoop, seen: Option<Seen>, outcome: String, new: String) -> Result<()> {
        let plan = &self.plan[self.idx];
        let phase = |marker: &str| new.lines().find(|l| l.contains(marker)).and_then(|l| field(l, "since_request_ms")).unwrap_or_else(|| "-1".into());
        let (checkpoint_ms, attached_ms) = (phase("capsule attach checkpoint applied"), phase("session pane: capsule attached"));
        if self.role == "fault" {
            let failed = new.contains(FAILURE);
            let sentinel = seen.as_ref().is_some_and(|s| s.sentinel);
            let ok = failed && sentinel;
            println!("pane-timing fault frame_failed_before_present={failed} order=failure-then-receipt sentinel={sentinel} ok={ok}");
        } else {
            let (total, driver, sentinel) = seen.as_ref().map_or(("-".to_string(), "-".to_string(), "-".to_string()), |s| (s.total_ns.to_string(), s.driver_ns.to_string(), if plan.resume { "-".to_string() } else { s.sentinel.to_string() }));
            let open_ms = seen.as_ref().and_then(|s| s.open_ms).map_or("-1".to_string(), |v| v.to_string());
            let kind = if plan.resume { "resume" } else { "ready" };
            println!(
                "pane-timing sample run={} kind={kind} row={} total_ns={total} driver_ns={driver} open_ms={open_ms} checkpoint_ms={checkpoint_ms} attached_ms={attached_ms} sentinel={sentinel} outcome={outcome}",
                &self.role, plan.slug
            );
            if seen.is_none() {
                println!("pane-timing pane lines for {}:\n{}", plan.slug, self.cells());
            }
        }
        self.cur = None;
        self.idx += 1;
        if self.idx < self.plan.len() {
            return Ok(());
        }
        self.finished = true;
        println!("pane-timing run={} completed", self.role.trim_start_matches("run-"));
        if let Some(state) = self.app.state.as_ref() {
            let id = state.window.id();
            self.app.window_event(event_loop, id, WindowEvent::CloseRequested);
        }
        Ok(())
    }

    fn after(&mut self, event_loop: &ActiveEventLoop) {
        if self.failure.is_some() || self.finished {
            return;
        }
        if let Err(e) = self.step(event_loop) {
            self.failure = Some(e);
            self.stop.store(true, Ordering::SeqCst);
            event_loop.exit();
        }
    }
}

impl ApplicationHandler for PaneDriver<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.app.resumed(event_loop);
        if self.app.state.is_none() {
            println!("pane-timing not runnable here: display: native App/State startup did not complete");
            self.stop.store(true, Ordering::SeqCst);
            event_loop.exit();
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.app.window_event(event_loop, id, event);
        self.after(event_loop);
    }
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.app.new_events(event_loop, cause);
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.about_to_wait(event_loop);
        self.after(event_loop);
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, (): ()) {
        self.after(event_loop);
    }
}

fn plan_for(role: &str, scenario: &Scenario) -> Result<Vec<Plan>> {
    let plan = |v: &RowSpec, resume| Plan { slug: v.slug.clone(), nonce: v.nonce.clone(), session: v.session.clone(), resume };
    if role == "fault" {
        return Ok(scenario.ready.iter().take(1).map(|v| plan(v, false)).collect());
    }
    let n: usize = role.trim_start_matches("run-").parse()?;
    let mut out: Vec<Plan> = scenario.ready.iter().map(|v| plan(v, false)).collect();
    out.push(plan(scenario.seeded.get(n - 1).ok_or_else(|| anyhow::anyhow!("no seeded row for {role}"))?, true));
    Ok(out)
}

fn child(role: &str, scenario_path: &std::path::Path) -> Result<()> {
    let scenario: Scenario = serde_json::from_slice(&std::fs::read(scenario_path)?).map_err(|e| anyhow::anyhow!("pane-timing scenario malformed: {e}"))?;
    println!("pane-timing: body entered");
    let _home = FixtureHome::enter()?;
    // The parent holds this pipe's write end for the child's whole life; its end is the parent's. The exit skips
    // the drops, so the watcher removes the fixture root itself first.
    let root = std::path::PathBuf::from(std::env::var_os("XDG_STATE_HOME").ok_or_else(|| anyhow::anyhow!("the fixture root is not set"))?);
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut sink);
        // The parent's reader is gone too, so this write may fail; it must not panic before the exit.
        let _ = writeln!(std::io::stdout(), "pane-timing parent gone");
        let _ = std::fs::remove_dir_all(&root);
        std::process::exit(70);
    });
    let capture = sot_log::test_log::capture();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info.to_string();
        if ["Surface::configure", "device is lost", "adapter"].iter().any(|w| message.contains(w)) {
            println!("pane-timing not runnable here: display: {}", message.lines().next().unwrap_or_default());
            let _ = std::io::stdout().flush();
        }
        previous(info);
    }));
    let event_loop = EventLoop::new().map_err(|e| anyhow::anyhow!("pane-timing not runnable here: display: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy: EventLoopProxy<()> = event_loop.create_proxy();
    let recipe = recipe_from(&scenario.host, std::path::Path::new(&scenario.program), &scenario.args);
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (req_tx, req_rx) = crate::net::transport::outgoing_channel();
    let config = crate::net::transport::TransportConfig { dial: crate::net::transport::Dial::Ssh(recipe), token: None };
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build()?;
    let mut app = App::new(evt_rx, Some(rt), fixture_cli(), evt_tx, vec![(HOST_KEY.to_string(), req_tx)], Some(vec![(HOST_KEY.to_string(), config, req_rx)]), crate::lease::Leases::new(true, Vec::new()));
    let inputs = crate::ui::init::NativeStartupInputs {
        resume: crate::ui::persist::resume::GlobalState { window_w: Some(1024.0), window_h: Some(768.0), ..Default::default() },
        topology: None,
        settings: Settings::default(),
        keybindings: KeyBindings::defaults(),
        ledger: Arc::new(std::sync::Mutex::new(NativeProgressLedger::default())),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let ticks = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) && proxy.send_event(()).is_ok() {
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let plan = plan_for(role, &scenario)?;
    let (mut failure, mut finished) = (None, false);
    let result = crate::ui::init::with_native_startup(inputs, || {
        app.run_with(|app| {
            let mut driver = PaneDriver { app, capture: &capture, role: role.to_string(), plan, idx: 0, cur: None, begun: Instant::now(), targets: Vec::new(), stop: stop.clone(), finished: false, failure: None };
            let result = event_loop.run_app(&mut driver);
            failure = driver.failure.take();
            finished = driver.finished;
            result
        })
    });
    stop.store(true, Ordering::SeqCst);
    let _ = ticks.join();
    result?;
    if let Some(error) = failure {
        return Err(error);
    }
    anyhow::ensure!(finished, "pane-timing child failure: the window closed before every sample");
    Ok(())
}
