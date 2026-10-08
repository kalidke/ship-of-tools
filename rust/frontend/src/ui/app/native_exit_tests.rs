//! The opt-in main-thread native window-close fixture: the actual App callbacks, State and returning-loop
//! finalizer, driven from test-owned inputs. The parent runs each case as a child of its binary.

use super::tests::{fixture_cli, FixtureHome};
use super::*;
use crate::ui::input::global_keys::confirm_quit_key;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use winit::event_loop::{EventLoop, EventLoopProxy};

const ROLE: &str = "SOT_TEST_T1_NATIVE_ROLE";
const CAPTURE: &str = "SOT_TEST_T1_CAPTURE";
const BACKSTOP_EVENT: &str = "final teardown exceeded its bound";
const ORDINARY_BOUND_MS: u128 = 2500;
/// The held acknowledgement outlasts the three-second backstop; the second-close case never gets one.
const HELD_ACK: Duration = Duration::from_millis(3600);

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    /// Exit 0 inside the ordinary bound with no backstop event.
    Ordinary,
    /// The decided code, no backstop event, and at least this many ms alive after the decision.
    Code(i32, u128),
    /// The decided code through the backstop, with the stall barrier entered.
    Backstop(i32),
}

/// (role, held stage, expectation)
const CASES: [(&str, Option<&str>, Expect); 11] = [
    ("close-os", None, Expect::Ordinary),
    ("quit-no", None, Expect::Ordinary),
    ("close-os", Some("returned"), Expect::Backstop(0)),
    ("relaunch-75", None, Expect::Code(75, 0)),
    ("relaunch-76", None, Expect::Code(76, 0)),
    ("relaunch-75", Some("direct"), Expect::Backstop(75)),
    ("held-ack", None, Expect::Code(0, 3600)),
    ("second-close", None, Expect::Ordinary),
    ("second-close", Some("delivery"), Expect::Backstop(0)),
    ("capture", None, Expect::Code(0, 0)),
    ("capture", Some("returned"), Expect::Backstop(0)),
];

/// Parent and child run the same entry; the role variable selects which.
pub(crate) fn run_native_window_close() -> Result<()> {
    match std::env::var(ROLE) {
        Ok(role) => child(&role),
        Err(_) => parent(),
    }
}

fn parent() -> Result<()> {
    for (role, stage, expect) in CASES {
        run_case(role, stage, expect)?;
        println!("window-close case={role} held={stage:?} ok=true");
    }
    println!("window-close completed_bodies={}", CASES.len());
    Ok(())
}

fn run_case(role: &str, stage: Option<&str>, expect: Expect) -> Result<()> {
    let folder = std::env::temp_dir().join(format!("sot-t1-capture-{}-{role}-{}", std::process::id(), stage.unwrap_or("none")));
    std::fs::create_dir_all(&folder)?;
    let capture = folder.join("shot.png");
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.env(ROLE, role).env(CAPTURE, &capture).stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    match stage {
        Some(s) => command.env("SOT_TEST_T1_BARRIER", s),
        None => command.env_remove("SOT_TEST_T1_BARRIER"),
    };
    let started = Instant::now();
    let child = command.spawn()?;
    let pid = child.id();
    let (status, out, err) = sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(60));
    let wall = started.elapsed();
    let shot = capture.exists();
    std::fs::remove_dir_all(&folder)?;
    if out.contains("not runnable here") || err.contains("not runnable here") {
        anyhow::bail!("window-close not runnable here: {role}: {out}{err}");
    }
    anyhow::ensure!(out.matches("native: body entered").count() == 1, "{role}: the body did not enter exactly once: {out}{err}");
    if role != "capture" && !(out.contains("native: frame presented") && out.contains("native: terminal decision")) {
        // No desktop, or a lost device: nothing rendered, so nothing was proved. Never a pass.
        anyhow::bail!("window-close not runnable here: {role}: no rendered frame preceded the decision (status {status:?}): {out}{err}");
    }
    if role == "capture" && !shot && !matches!(expect, Expect::Backstop(_)) {
        anyhow::bail!("window-close not runnable here: {role}: no capture was written (status {status:?}): {out}{err}");
    }
    let want_code = match expect {
        Expect::Ordinary => 0,
        Expect::Code(c, _) | Expect::Backstop(c) => c,
    };
    anyhow::ensure!(status.code() == Some(want_code), "{role}: exit status {status:?}, wanted {want_code}: {out}{err}");
    if role == "capture" {
        anyhow::ensure!(shot, "{role}: the capture was never written: {out}");
    }
    match expect {
        Expect::Backstop(_) => {
            anyhow::ensure!(out.contains(&format!("barrier entered: {}", stage.unwrap())), "{role}: stall barrier never entered: {out}");
            anyhow::ensure!(out.contains(BACKSTOP_EVENT), "{role}: stalled teardown did not end through the backstop: {out}");
        }
        Expect::Ordinary | Expect::Code(..) => {
            anyhow::ensure!(!out.contains(BACKSTOP_EVENT), "{role}: ordinary teardown tripped the backstop: {out}");
            let ms = elapsed_ms(&out);
            if role != "capture" && role != "relaunch-75" && role != "relaunch-76" {
                let ms = ms.ok_or_else(|| anyhow::anyhow!("{role}: no close timing: {out}"))?;
                if let Expect::Code(_, least) = expect {
                    anyhow::ensure!(ms >= least, "{role}: closed after {ms} ms, before its held acknowledgement ({least} ms)");
                } else {
                    anyhow::ensure!(ms < ORDINARY_BOUND_MS, "{role}: ordinary close took {ms} ms");
                }
            }
        }
    }
    if role == "held-ack" {
        anyhow::ensure!(out.contains("fe.leaving") && out.contains("fe.notice_seen"),
            "{role}: the daemon did not see the leave and the presented notice acknowledged: {out}");
    }
    #[cfg(windows)]
    if !matches!(expect, Expect::Backstop(_)) {
        let image = std::env::current_exe()?.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let hangs = super::native_eventlog::hang_events_for(&image, pid, wall.as_millis() as u64 + 5000)?;
        anyhow::ensure!(hangs.is_empty(), "{role}: Application Hang event for the fixture: {hangs:?}");
        println!("window-close case={role} application_hang_events=0");
    }
    #[cfg(not(windows))]
    let _ = (pid, wall);
    Ok(())
}

fn elapsed_ms(out: &str) -> Option<u128> {
    out.lines().find_map(|l| l.strip_prefix("native: closed elapsed_ms=")).and_then(|v| v.trim().parse().ok())
}

/// One native window in this process: wait for a rendered frame, then make the terminal decision.
struct CloseDriver<'a> {
    app: &'a mut App,
    role: String,
    decided: Option<Instant>,
    second_due: Option<Instant>,
    stop: std::sync::Arc<AtomicBool>,
    failure: Option<anyhow::Error>,
}

impl CloseDriver<'_> {
    fn decide(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.app.state.as_mut() else { return };
        if state.frame_counter < 3 || self.role == "capture" {
            return;
        }
        println!("native: frame presented");
        let id = state.window.id();
        let first = Instant::now();
        match self.role.as_str() {
            "close-os" | "held-ack" => {
                println!("native: terminal decision");
                self.decided = Some(first);
                self.app.window_event(event_loop, id, WindowEvent::CloseRequested);
            }
            "second-close" => {
                // The first close starts a Close that waits for a held acknowledgement; the second is terminal.
                self.second_due = Some(first + Duration::from_millis(300));
                self.decided = Some(first);
                self.app.window_event(event_loop, id, WindowEvent::CloseRequested);
            }
            "relaunch-75" | "relaunch-76" => {
                println!("native: terminal decision");
                self.decided = Some(first);
                state.relaunch_flag.store(if self.role.ends_with("75") { 75 } else { 76 }, Ordering::Relaxed);
                self.app.window_event(event_loop, id, WindowEvent::Focused(true));
            }
            _ => {
                println!("native: terminal decision");
                self.decided = Some(first);
                state.request_quit(event_loop, ExitReason::QuitKey);
                if !matches!(state.nav_prompt, Some(NavPrompt::ConfirmQuit { .. })) && self.failure.is_none() {
                    self.failure = Some(anyhow::anyhow!("Ctrl+Q opened no prompt"));
                }
                confirm_quit_key(state, event_loop, quit_prompt_step(false, &Key::Named(NamedKey::Enter), false));
            }
        }
    }

    fn second_close(&mut self, event_loop: &ActiveEventLoop) {
        let Some(due) = self.second_due.filter(|d| Instant::now() >= *d) else { return };
        self.second_due = None;
        let Some(state) = self.app.state.as_ref() else { return };
        let id = state.window.id();
        println!("native: terminal decision");
        self.decided = Some(Instant::now());
        self.app.window_event(event_loop, id, WindowEvent::CloseRequested);
        let _ = due;
    }
}

impl ApplicationHandler for CloseDriver<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.app.resumed(event_loop);
        if self.app.state.is_none() {
            println!("native: not runnable here: native App/State startup did not complete");
            self.stop.store(true, Ordering::SeqCst);
            event_loop.exit();
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.app.window_event(event_loop, id, event);
    }
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.app.new_events(event_loop, cause);
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.about_to_wait(event_loop);
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, (): ()) {
        if self.second_due.is_some() {
            self.second_close(event_loop);
        } else if self.decided.is_none() {
            self.decide(event_loop);
        }
    }
}

/// A runtime holding a fake daemon whose lease is already granted; its reply to a Close is held for `delay`.
fn leased(delay: Duration) -> Result<(tokio::runtime::Runtime, std::sync::Arc<crate::lease::Leases>, std::sync::Arc<std::sync::Mutex<Vec<String>>>)> {
    use crate::lease::{grant_tests::bind, leave_tests::leave_fake};
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build()?;
    let host = "fixture".to_string();
    let leases = crate::lease::Leases::new(false, vec![host.clone()]);
    let log = rt.block_on(async {
        let (listener, path) = bind("t1native");
        let (log, _, _fake) = leave_fake(listener, None, Some("close"), delay);
        tokio::time::timeout(Duration::from_secs(5), leases.before_data_connection(&host, &path, None)).await??;
        anyhow::Ok(log)
    })?;
    Ok((rt, leases, log))
}

fn child(role: &str) -> Result<()> {
    println!("native: body entered");
    let _home = FixtureHome::enter()?;
    let event_loop = EventLoop::new().map_err(|e| anyhow::anyhow!("not runnable here: {e}"))?;
    let capture = role == "capture";
    event_loop.set_control_flow(if capture { ControlFlow::Poll } else { ControlFlow::Wait });
    let proxy: EventLoopProxy<()> = event_loop.create_proxy();
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (req_tx, _req_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut cli = fixture_cli();
    if capture {
        cli.capture = std::env::var_os(CAPTURE).map(std::path::PathBuf::from);
    }
    let (rt, leases, log) = match role {
        "held-ack" => leased(HELD_ACK).map(|(r, l, g)| (Some(r), l, Some(g)))?,
        "second-close" => leased(Duration::from_secs(30)).map(|(r, l, g)| (Some(r), l, Some(g)))?,
        _ => (None, crate::lease::Leases::new(true, Vec::new()), None),
    };
    let mut app = App::new(evt_rx, rt, cli, evt_tx, vec![("<host>".to_string(), req_tx)], None, leases);
    let inputs = crate::ui::init::NativeStartupInputs {
        resume: crate::ui::persist::resume::GlobalState { window_w: Some(640.0), window_h: Some(480.0), ..Default::default() },
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
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let (mut decided, mut failure) = (None, None);
    let result = crate::ui::init::with_native_startup(inputs, || {
        app.run_with(|app| {
            let mut driver = CloseDriver { app, role: role.to_string(), decided: None, second_due: None, stop: stop.clone(), failure: None };
            let result = event_loop.run_app(&mut driver);
            decided = driver.decided;
            failure = driver.failure.take();
            result
        })
    });
    stop.store(true, Ordering::SeqCst);
    let _ = ticks.join();
    result?;
    if let Some(error) = failure {
        return Err(error);
    }
    if let Some(log) = log {
        for entry in log.lock().unwrap().iter() {
            println!("native: daemon saw {entry}");
        }
    }
    let closed = match decided {
        Some(at) => at.elapsed().as_millis(),
        None if capture => 0,
        None => anyhow::bail!("not runnable here: no frame was presented"),
    };
    println!("native: closed elapsed_ms={closed}");
    Ok(())
}
