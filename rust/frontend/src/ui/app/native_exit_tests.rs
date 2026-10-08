//! The opt-in main-thread native window-close fixture: the actual App callbacks, State and returning-loop
//! finalizer, driven from test-owned inputs. The parent runs each case as a child of this binary.

use super::tests::{fixture_cli, FixtureHome};
use super::*;
use crate::ui::input::global_keys::confirm_quit_key;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use winit::event_loop::{EventLoop, EventLoopProxy};

const ROLE: &str = "SOT_TEST_T1_NATIVE_ROLE";
const BACKSTOP_EVENT: &str = "final teardown exceeded its bound";
const ORDINARY_BOUND_MS: u128 = 2500;

/// Parent and child run the same entry; the role variable selects which.
pub(crate) fn run_native_window_close() -> Result<()> {
    match std::env::var(ROLE) {
        Ok(role) => child(&role),
        Err(_) => parent(),
    }
}

fn parent() -> Result<()> {
    // (role, held stage, expect the backstop)
    let cases = [("close-os", None, false), ("quit-no", None, false), ("close-os", Some("returned"), true)];
    for (role, stage, sabotaged) in cases {
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command.env(ROLE, role).stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        match stage {
            Some(s) => command.env("SOT_TEST_T1_BARRIER", s),
            None => command.env_remove("SOT_TEST_T1_BARRIER"),
        };
        let child = command.spawn()?;
        let (status, out, err) = sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(60));
        if out.contains("not runnable here") || err.contains("not runnable here") {
            anyhow::bail!("not runnable here: {role}: {out}{err}");
        }
        anyhow::ensure!(out.matches("native: body entered").count() == 1, "{role}: the body did not enter exactly once: {out}{err}");
        anyhow::ensure!(out.contains("native: frame presented") && out.contains("native: terminal decision"),
            "{role}: no rendered frame preceded the decision: {out}");
        anyhow::ensure!(status.code() == Some(0), "{role}: exit status {status:?}: {out}{err}");
        if sabotaged {
            anyhow::ensure!(out.contains(&format!("barrier entered: {}", stage.unwrap())), "{role}: stall barrier never entered: {out}");
            anyhow::ensure!(out.contains(BACKSTOP_EVENT), "{role}: stalled teardown did not end through the backstop: {out}");
        } else {
            anyhow::ensure!(!out.contains(BACKSTOP_EVENT), "{role}: ordinary close tripped the backstop: {out}");
            let ms = out.lines().find_map(|l| l.strip_prefix("native: closed elapsed_ms=")).and_then(|v| v.trim().parse::<u128>().ok());
            let ms = ms.ok_or_else(|| anyhow::anyhow!("{role}: no close timing: {out}"))?;
            anyhow::ensure!(ms < ORDINARY_BOUND_MS, "{role}: ordinary close took {ms} ms");
        }
        println!("window-close case={role} held={stage:?} ok=true");
    }
    println!("window-close completed_bodies=3");
    Ok(())
}

/// One native window in this process: wait for a rendered frame, then make the terminal decision.
struct CloseDriver<'a> {
    app: &'a mut App,
    role: String,
    decided: Option<Instant>,
    stop: std::sync::Arc<AtomicBool>,
    failure: Option<anyhow::Error>,
}

impl CloseDriver<'_> {
    fn decide(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.app.state.as_mut() else { return };
        if state.frame_counter < 3 {
            return;
        }
        println!("native: frame presented");
        println!("native: terminal decision");
        self.decided = Some(Instant::now());
        if self.role == "close-os" {
            let id = state.window.id();
            self.app.window_event(event_loop, id, WindowEvent::CloseRequested);
        } else {
            state.request_quit(event_loop, ExitReason::QuitKey);
            anyhow_ensure(&mut self.failure, matches!(state.nav_prompt, Some(NavPrompt::ConfirmQuit { .. })), "Ctrl+Q opened no prompt");
            confirm_quit_key(state, event_loop, quit_prompt_step(false, &Key::Named(NamedKey::Enter), false));
        }
    }
}

fn anyhow_ensure(slot: &mut Option<anyhow::Error>, ok: bool, what: &str) {
    if !ok && slot.is_none() {
        *slot = Some(anyhow::anyhow!("{what}"));
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
        if self.decided.is_none() {
            self.decide(event_loop);
        }
    }
}

fn child(role: &str) -> Result<()> {
    println!("native: body entered");
    let _home = FixtureHome::enter()?;
    let event_loop = EventLoop::new().map_err(|e| anyhow::anyhow!("not runnable here: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy: EventLoopProxy<()> = event_loop.create_proxy();
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (req_tx, _req_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(evt_rx, None, fixture_cli(), evt_tx, vec![("<host>".to_string(), req_tx)], None,
        crate::lease::Leases::new(true, Vec::new()));
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
    let mut decided = None;
    let mut failure = None;
    let result = crate::ui::init::with_native_startup(inputs, || {
        app.run_with(|app| {
            let mut driver = CloseDriver { app, role: role.to_string(), decided: None, stop: stop.clone(), failure: None };
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
    let decided = decided.ok_or_else(|| anyhow::anyhow!("not runnable here: no frame was presented"))?;
    println!("native: closed elapsed_ms={}", decided.elapsed().as_millis());
    Ok(())
}
