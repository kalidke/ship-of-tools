//! The window_minimized target's children and parent: the start its two children share, the State fixtures' own
//! child, and the parent that runs both.

use super::tests::{fixture_cli, FixtureHome, NativeProgressLedger, HOST};
use super::*;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use winit::event_loop::EventLoop;

const FIXTURES_BOUND: Duration = Duration::from_secs(120);
/// The three phases (2 s, 20 s, 600 s), their native transitions, the final drain and the first frames, with room.
const WORKLOAD_BOUND: Duration = Duration::from_secs(780);

/// The start the window_minimized target's two children share: the fixture home, the "not runnable here" panic hook,
/// an event loop and an App on one synthetic host with test-owned startup inputs. The close and pane children build
/// their own.
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
        MainWake::new(event_loop.create_proxy()),
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

/// The State fixtures of the result-routing, badge, account and canonical-row commits. They rewire State (its event
/// receiver, its connections, its active host), so they run in a child and window of their own, never on the
/// workload's State.
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
    let fixtures: [(&str, fn(&mut State) -> Result<()>); 4] = [
        ("result_target_uses_host_and_listed_identity", State::result_target_uses_host_and_listed_identity),
        ("passing_a_result_row_keeps_its_badge", State::passing_a_result_row_keeps_its_badge),
        ("first_named_account_is_sent", State::first_named_account_is_sent),
        ("pending_result_dies_with_its_canonical_row", State::pending_result_dies_with_its_canonical_row),
    ];
    // Every fixture runs and reports; the child fails after the last if any failed.
    let mut failed = Vec::new();
    for (name, fixture) in fixtures {
        let result = fixture(state);
        println!("state-fixture name={name} ok={}", result.is_ok());
        if let Err(error) = result {
            eprintln!("state-fixture name={name}: {error:#}");
            failed.push(name);
        }
    }
    anyhow::ensure!(failed.is_empty(), "State fixtures failed: {}", failed.join(", "));
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
/// window of its own. Their output streams straight through, so an overrun loses none of it. Both run whatever the
/// first did; then each must have exited 0 in its bound.
pub(in crate::ui) fn window_progress_parent() -> Result<()> {
    let mut failed = Vec::new();
    for (role, bound) in [("fixtures", FIXTURES_BOUND), ("workload", WORKLOAD_BOUND)] {
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args(["child", role])
            .stdin(std::process::Stdio::null())
            .spawn()?;
        match sot_log::test_isolated::wait_until(&mut child, Instant::now() + bound) {
            Ok(status) if status.success() => println!("window-progress child={role} ok=true"),
            Ok(status) => failed.push(format!("window-progress child {role} failed: {status}")),
            Err(e) => failed.push(format!("window-progress child {role} did not finish within {bound:?}: {e}")),
        }
    }
    anyhow::ensure!(failed.is_empty(), "{}", failed.join("; "));
    Ok(())
}
