//! The native window harnesses' children: the start every window child shares, the State fixtures' own child, the
//! window_minimized parent, and the one bounded wait that keeps a child's output when it overruns.

use super::tests::{fixture_cli, FixtureHome, NativeProgressLedger, HOST};
use super::*;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use winit::event_loop::EventLoop;

const FIXTURES_BOUND: Duration = Duration::from_secs(120);
/// The three phases (2 s, 20 s, 600 s), their native transitions, the final drain and the first frames, with room.
const WORKLOAD_BOUND: Duration = Duration::from_secs(780);

/// Every native window child's start: the fixture home, the "not runnable here" panic hook, an event loop and an App
/// on one synthetic host with test-owned startup inputs.
pub(super) struct NativeChild {
    pub(super) _home: FixtureHome,
    pub(super) event_loop: EventLoop<()>,
    pub(super) app: App,
    pub(super) outgoing: tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
    pub(super) inputs: crate::ui::init::NativeStartupInputs,
}

pub(super) fn native_child(ledger: Arc<Mutex<NativeProgressLedger>>) -> Result<NativeChild> {
    let home = FixtureHome::enter()?;
    // A session with no usable display or GPU fails inside wgpu with a panic; say it is not runnable here.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info.to_string();
        if ["Surface::configure", "device is lost", "wgpu"]
            .iter()
            .any(|w| message.contains(w))
        {
            println!(
                "window-progress not runnable here: {}",
                message.lines().next().unwrap_or_default()
            );
            let _ = std::io::stdout().flush();
        }
        previous(info);
    }));
    let event_loop = EventLoop::new().map_err(|e| anyhow::anyhow!("not runnable here: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (req_tx, outgoing) = tokio::sync::mpsc::unbounded_channel();
    let app = App::new(
        evt_rx,
        None,
        fixture_cli(),
        evt_tx,
        vec![(HOST.to_string(), req_tx)],
        None,
        crate::lease::Leases::new(true, Vec::new()),
    );
    let inputs = crate::ui::init::NativeStartupInputs {
        resume: crate::ui::persist::resume::GlobalState {
            window_w: Some(640.0),
            window_h: Some(480.0),
            ..Default::default()
        },
        topology: None,
        settings: Settings::default(),
        keybindings: KeyBindings::defaults(),
        ledger,
    };
    Ok(NativeChild { _home: home, event_loop, app, outgoing, inputs })
}

/// The State fixtures of the result-routing, badge and account commits. They rewire State (its event receiver, its
/// connections, its active host), so they run in a child and window of their own, never on the workload's State.
struct FixturesDriver {
    app: App,
    outcome: Option<Result<()>>,
}

impl ApplicationHandler for FixturesDriver {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.app.resumed(event_loop);
        if self.outcome.is_some() {
            return;
        }
        self.outcome = Some(match self.app.state.as_mut() {
            Some(state) => run_state_fixtures(state),
            None => Err(anyhow::anyhow!("not runnable here: native App/State startup did not complete")),
        });
        event_loop.exit();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.app.window_event(event_loop, id, event);
    }
}

fn run_state_fixtures(state: &mut State) -> Result<()> {
    let fixtures: [(&str, fn(&mut State) -> Result<()>); 3] = [
        ("result_target_uses_host_and_listed_identity", State::result_target_uses_host_and_listed_identity),
        ("passing_a_result_row_keeps_its_badge", State::passing_a_result_row_keeps_its_badge),
        ("first_named_account_is_sent", State::first_named_account_is_sent),
    ];
    for (name, fixture) in fixtures {
        let result = fixture(state);
        println!("state-fixture name={name} ok={}", result.is_ok());
        result.map_err(|error| error.context(name))?;
    }
    Ok(())
}

pub(in crate::ui) fn state_fixtures_child() -> Result<()> {
    let NativeChild { _home, event_loop, app, outgoing: _, inputs } =
        native_child(Arc::new(Mutex::new(NativeProgressLedger::default())))?;
    let mut driver = FixturesDriver { app, outcome: None };
    crate::ui::init::with_native_startup(inputs, || event_loop.run_app(&mut driver))?;
    driver.outcome.unwrap_or_else(|| Err(anyhow::anyhow!("the State fixtures exited without a verdict")))
}

/// The window_minimized target's parent: the State fixtures and then the workload, each a child of this binary with a
/// window of its own. Their output streams straight through, so an overrun loses none of it; each must exit 0 in its
/// bound.
pub(in crate::ui) fn window_progress_parent() -> Result<()> {
    for (role, bound) in [("fixtures", FIXTURES_BOUND), ("workload", WORKLOAD_BOUND)] {
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args(["child", role])
            .stdin(std::process::Stdio::null())
            .spawn()?;
        let status = sot_log::test_isolated::wait_until(&mut child, Instant::now() + bound)
            .map_err(|e| anyhow::anyhow!("window-progress child {role} did not finish within {bound:?}: {e}"))?;
        anyhow::ensure!(status.success(), "window-progress child {role} failed: {status}");
        println!("window-progress child={role} ok=true");
    }
    Ok(())
}

/// One native child's exit and output. `status` is `None` when the child overran its bound and was ended; what it
/// wrote until then is kept either way.
pub(super) struct Captured {
    pub(super) status: Option<std::process::ExitStatus>,
    pub(super) out: String,
    pub(super) err: String,
}

/// Reads `pipe` to its end on a thread of its own.
fn read_all(pipe: Option<impl std::io::Read + Send + 'static>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let mut bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut pipe, &mut bytes);
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        let _ = tx.send(text);
    });
    rx
}

/// Waits up to `bound` for `child`, whose stdout and stderr are piped, reading both meanwhile. A child that overruns
/// is ended (`test_isolated::wait_until`) and the reason joins its stderr, so the caller can print what it said.
pub(super) fn run_captured(mut child: std::process::Child, bound: Duration) -> Captured {
    let (out_rx, err_rx) = (read_all(child.stdout.take()), read_all(child.stderr.take()));
    let waited = sot_log::test_isolated::wait_until(&mut child, Instant::now() + bound);
    let mut err = err_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| "the child's stderr was still open 10 s after it ended\n".to_string());
    let out = out_rx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| {
        err.push_str("the child's stdout was still open 10 s after it ended\n");
        String::new()
    });
    let status = match waited {
        Ok(status) => Some(status),
        Err(e) => {
            err.push_str(&format!("the child did not finish within {bound:?}; cleanup: {e}\n"));
            None
        }
    };
    Captured { status, out, err }
}
