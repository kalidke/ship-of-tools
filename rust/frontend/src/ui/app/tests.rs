//! Native workload observations around the real App/State and transport loop.
//! The owned peer supplies events; only production callbacks drain them.

use super::*;
use crate::net::transport::{observe_native_fan_in, run_native_progress_transport, IncomingEvt};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use winit::event_loop::{EventLoop, EventLoopProxy};

const TEST: &str = "ui::app::tests::minimized_window_drains_events_for_ten_minutes";
const HOST: &str = "<host>";

#[derive(Clone, Copy, Debug)]
struct Profile {
    phase: &'static str,
    seconds: u32,
    expected: usize,
    minimized: bool,
    stall: bool,
}

const VISIBLE: Profile = Profile {
    phase: "visible",
    seconds: 2,
    expected: 40,
    minimized: false,
    stall: false,
};
const CONTROL: Profile = Profile {
    phase: "negative-control",
    seconds: 20,
    expected: 400,
    minimized: true,
    stall: true,
};
const MINIMIZED: Profile = Profile {
    phase: "minimized",
    seconds: 600,
    expected: 12000,
    minimized: true,
    stall: false,
};

struct PhaseLedger {
    profile: Profile,
    epoch: Instant,
    accepted: BTreeMap<usize, Instant>,
    dequeued: BTreeMap<usize, Instant>,
    max_outstanding: usize,
    native_ok: bool,
    samples: usize,
    producer_done: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct NativeProgressLedger {
    phases: BTreeMap<String, PhaseLedger>,
}

impl NativeProgressLedger {
    fn begin(&mut self, profile: Profile) -> Instant {
        let epoch = Instant::now();
        assert!(
            self.phases
                .insert(
                    profile.phase.to_string(),
                    PhaseLedger {
                        profile,
                        epoch,
                        accepted: BTreeMap::new(),
                        dequeued: BTreeMap::new(),
                        max_outstanding: 0,
                        native_ok: true,
                        samples: 0,
                        producer_done: None,
                    }
                )
                .is_none(),
            "a workload phase entered twice"
        );
        epoch
    }

    pub(crate) fn tag(&self, host: &HostKey, event: &IncomingEvt) -> Option<(String, usize)> {
        let IncomingEvt::Event { op, payload } = event else {
            return None;
        };
        if host != HOST || op != "fixture.window-progress" {
            return None;
        }
        let phase = payload.get("phase")?.as_str()?.to_string();
        let seq = payload.get("sequence")?.as_u64()? as usize;
        Some((phase, seq))
    }

    pub(crate) fn accepted(&mut self, tag: Option<(String, usize)>) {
        let Some((phase, seq)) = tag else { return };
        let now = Instant::now();
        let state = self
            .phases
            .get_mut(&phase)
            .expect("event has a started phase");
        assert!(
            (1..=state.profile.expected).contains(&seq),
            "workload sequence outside its profile"
        );
        assert!(
            state.accepted.insert(seq, now).is_none(),
            "duplicate successful fan-in send"
        );
        state.observe_outstanding(now);
        println!(
            "window-progress event=accepted profile={phase} sequence={seq} elapsed_ms={}",
            now.duration_since(state.epoch).as_millis()
        );
    }

    pub(crate) fn dequeued(&mut self, host: &HostKey, event: &IncomingEvt) {
        let Some((phase, seq)) = self.tag(host, event) else {
            return;
        };
        let now = Instant::now();
        let state = self
            .phases
            .get_mut(&phase)
            .expect("dequeue has a started phase");
        assert!(
            state.accepted.contains_key(&seq),
            "a dequeue preceded accepted-send accounting"
        );
        assert!(
            state.dequeued.insert(seq, now).is_none(),
            "duplicate actual State dequeue"
        );
        state.observe_outstanding(now);
        println!(
            "window-progress event=dequeued profile={phase} sequence={seq} elapsed_ms={}",
            now.duration_since(state.epoch).as_millis()
        );
    }
}

impl PhaseLedger {
    fn outstanding(&self) -> usize {
        self.accepted.len() - self.dequeued.len()
    }

    fn observe_outstanding(&mut self, now: Instant) {
        if now.duration_since(self.epoch) >= Duration::from_secs(5) {
            self.max_outstanding = self.max_outstanding.max(self.outstanding());
        }
    }

    fn rate(&self, elapsed: Duration) -> (usize, usize, u128) {
        let full_seconds = elapsed.as_secs().min(self.profile.seconds as u64);
        let windows: Vec<usize> = (0..=full_seconds.saturating_sub(10))
            .filter(|_| full_seconds >= 10)
            .map(|offset| {
                self.accepted
                    .values()
                    .filter(|&&at| {
                        let t = at.duration_since(self.epoch);
                        t >= Duration::from_secs(offset) && t < Duration::from_secs(offset + 10)
                    })
                    .count()
            })
            .collect();
        let lateness = self
            .accepted
            .iter()
            .map(|(&seq, &at)| {
                at.saturating_duration_since(self.epoch + Duration::from_millis(seq as u64 * 50))
                    .as_millis()
            })
            .max()
            .unwrap_or(0);
        (
            windows.iter().copied().min().unwrap_or(0),
            windows.iter().copied().max().unwrap_or(0),
            lateness,
        )
    }

    fn failures(&self, now: Instant) -> Vec<&'static str> {
        let mut failures = Vec::new();
        let elapsed = now.duration_since(self.epoch);
        let (min, max, late) = self.rate(elapsed);
        if self.accepted.len() != self.profile.expected {
            failures.push("workload_volume");
        }
        if late > 250 || (self.profile.seconds >= 10 && (min < 190 || max > 210)) {
            failures.push("workload_rate");
        }
        if self.dequeued.len() != self.accepted.len()
            || self.outstanding() != 0
            || self.max_outstanding > 200
        {
            failures.push("queue_progress");
        }
        if self.samples < self.profile.seconds as usize
            || !self.native_ok
            || elapsed.as_secs() < self.profile.seconds as u64
        {
            failures.push("native_observation");
        }
        if self.producer_done.is_none()
            || self
                .dequeued
                .values()
                .any(|&at| at > self.producer_done.unwrap() + Duration::from_secs(5))
        {
            failures.push("final_drain");
        }
        failures
    }

    fn print(&self, phase: &str, now: Instant, native: bool) {
        let (min, max, late) = self.rate(now.duration_since(self.epoch));
        let workload_ok = self.accepted.len() == self.profile.expected
            && late <= 250
            && (self.profile.seconds < 10 || (min >= 190 && max <= 210));
        println!("window-progress test={TEST} phase={phase} elapsed_s={} expected_sent={} sent={} dequeued={} outstanding={} max_outstanding={} min_10s_sent={min} max_10s_sent={max} max_lateness_ms={late} native_minimized={native} workload_ok={workload_ok}",
            now.duration_since(self.epoch).as_secs(), self.profile.expected, self.accepted.len(), self.dequeued.len(), self.outstanding(), self.max_outstanding);
        std::io::stdout()
            .flush()
            .expect("flush native progress proof");
    }
}

async fn produce(
    mut peer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    mut profiles: tokio::sync::mpsc::UnboundedReceiver<(Profile, Instant)>,
    ledger: Arc<Mutex<NativeProgressLedger>>,
) -> Result<()> {
    while let Some((profile, epoch)) = profiles.recv().await {
        for seq in 1..=profile.expected {
            let deadline = epoch + Duration::from_millis(seq as u64 * 50);
            tokio::time::sleep_until(deadline.into()).await;
            if profile.stall && (100..200).contains(&seq) {
                continue;
            }
            let frame = sot_protocol::Frame::evt(
                "fixture.window-progress",
                serde_json::json!({
                    "phase": profile.phase, "sequence": seq,
                }),
            );
            assert!(
                frame.rev.is_none(),
                "the fixture must not request persistence"
            );
            sot_protocol::codec::write_frame(&mut peer, &frame, None).await?;
        }
        // The final event is complete only when the real fan-in accepts it.
        let deadline = epoch + Duration::from_secs(profile.seconds as u64) + Duration::from_secs(1);
        loop {
            {
                let mut state = ledger.lock().expect("native progress ledger");
                let state = state
                    .phases
                    .get_mut(profile.phase)
                    .expect("started producer phase");
                if state.accepted.contains_key(&profile.expected) {
                    state.producer_done = Some(Instant::now());
                    break;
                }
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "the final scheduled event did not reach fan-in"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    peer.shutdown().await?;
    Ok(())
}

struct PeerTasks {
    rt: tokio::runtime::Runtime,
    profiles: Option<tokio::sync::mpsc::UnboundedSender<(Profile, Instant)>>,
    tasks: Vec<tokio::task::JoinHandle<Result<()>>>,
}

impl PeerTasks {
    fn start(
        window: Arc<Window>,
        sender: std::sync::mpsc::Sender<(HostKey, IncomingEvt)>,
        mut outgoing: tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        ledger: Arc<Mutex<NativeProgressLedger>>,
        proxy: EventLoopProxy<()>,
    ) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let (frontend, peer) = tokio::io::duplex(4096);
        let (rx, tx) = tokio::io::split(frontend);
        let (mut peer_rx, peer_tx) = tokio::io::split(peer);
        let (profiles, profile_rx) = tokio::sync::mpsc::unbounded_channel();
        let transport = rt.spawn(async move {
            let result = run_native_progress_transport(
                BufReader::new(rx),
                tx,
                HOST.to_string(),
                &sender,
                &mut outgoing,
                &window,
            )
            .await;
            // EOF of this owned synthetic peer is the expected terminal result.
            match result {
                Err(e) if e.to_string() == "eof" => Ok(()),
                other => other,
            }
        });
        let producer = rt.spawn(produce(peer_tx, profile_rx, ledger));
        let drain = rt.spawn(async move {
            tokio::io::copy(&mut peer_rx, &mut tokio::io::sink()).await?;
            Ok(())
        });
        let ticker = rt.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                if proxy.send_event(()).is_err() {
                    return Ok(());
                }
            }
        });
        Ok(Self {
            rt,
            profiles: Some(profiles),
            tasks: vec![transport, producer, drain, ticker],
        })
    }

    fn stop(mut self) -> Result<()> {
        self.profiles.take();
        for task in &self.tasks {
            task.abort();
        }
        self.rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                for task in self.tasks {
                    match task.await {
                        Ok(result) => result?,
                        Err(error) if error.is_cancelled() => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(())
            })
            .await?
        })
    }
}

struct NativeDriver {
    app: App,
    ledger: Arc<Mutex<NativeProgressLedger>>,
    proxy: EventLoopProxy<()>,
    outgoing: Option<tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>>,
    peer: Option<PeerTasks>,
    current: Option<Profile>,
    next: Option<Profile>,
    transition: Instant,
    outcome: Option<Result<()>>,
}

impl NativeDriver {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.outcome = Some(Err(error));
        event_loop.exit();
    }

    fn begin(&mut self, profile: Profile, native: bool) {
        let mut ledger = self.ledger.lock().expect("native progress ledger");
        let epoch = ledger.begin(profile);
        let state = &ledger.phases[profile.phase];
        state.print("body-entered", epoch, native);
        self.peer
            .as_ref()
            .expect("started peer")
            .profiles
            .as_ref()
            .expect("profile sender")
            .send((profile, epoch))
            .expect("producer task accepting profiles");
        self.current = Some(profile);
    }

    fn tick(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        anyhow::ensure!(
            self.peer
                .as_ref()
                .is_some_and(|peer| peer.tasks.iter().all(|task| !task.is_finished())),
            "an owned native peer task finished before the proof completed"
        );
        let Some(state) = self.app.state.as_ref() else {
            anyhow::bail!("not runnable here: native App/State startup did not complete")
        };
        let native = state.window.is_minimized().ok_or_else(|| {
            anyhow::anyhow!("not runnable here: native minimized state unavailable")
        })?;
        let now = Instant::now();
        if let Some(next) = self.next {
            if native == next.minimized {
                self.next = None;
                self.begin(next, native);
            } else {
                anyhow::ensure!(
                    now.duration_since(self.transition) < Duration::from_secs(10),
                    "not runnable here: native minimization was not confirmed"
                );
            }
            return Ok(());
        }
        let profile = self.current.expect("a current native phase");
        let mut ledger = self.ledger.lock().expect("native progress ledger");
        let phase = ledger
            .phases
            .get_mut(profile.phase)
            .expect("started current phase");
        phase.native_ok &= native == profile.minimized;
        phase.samples += 1;
        phase.observe_outstanding(now);
        phase.print("sample", now, native);
        let Some(done) = phase.producer_done else {
            return Ok(());
        };
        if phase.outstanding() != 0 && now.duration_since(done) < Duration::from_secs(5) {
            return Ok(());
        }
        let failures = phase.failures(now);
        phase.print("completed", now, native);
        if profile.stall {
            anyhow::ensure!(
                failures.contains(&"workload_volume")
                    && failures.contains(&"workload_rate")
                    && !failures.contains(&"native_observation"),
                "stalled-producer control did not settle its two workload predicates: {failures:?}"
            );
            println!("window-progress control=stalled_minimized_producer_is_rejected expected_failure=workload_volume,workload_rate observed_failure={} control_bodies=1", failures.join(","));
        } else {
            anyhow::ensure!(
                failures.is_empty(),
                "native {} proof failed: {}",
                profile.phase,
                failures.join(",")
            );
        }
        drop(ledger);
        if profile.phase == MINIMIZED.phase {
            println!("window-progress phase=completed expected_sent=12000 sent=12000 dequeued=12000 outstanding=0 workload_ok=true completed_bodies=1");
            std::io::stdout().flush()?;
            self.outcome = Some(Ok(()));
            event_loop.exit();
        } else {
            let next = if profile.phase == VISIBLE.phase {
                CONTROL
            } else {
                MINIMIZED
            };
            self.app
                .state
                .as_ref()
                .expect("native State")
                .window
                .set_minimized(next.minimized);
            self.next = Some(next);
            self.transition = now;
        }
        Ok(())
    }
}

impl ApplicationHandler for NativeDriver {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let sender = self.app.evt_tx.clone();
        self.app.resumed(event_loop);
        if self.peer.is_some() {
            return;
        }
        let Some(_) = self.app.state.as_ref() else {
            self.fail(
                event_loop,
                anyhow::anyhow!("not runnable here: native App/State startup did not complete"),
            );
            return;
        };
        // The behaviour proofs of the result-routing, badge and account commits need a real State.
        let state = self.app.state.as_mut().expect("native State");
        let fixtures: [(&str, fn(&mut State) -> Result<()>); 3] = [
            (
                "result_target_uses_host_and_listed_identity",
                State::result_target_uses_host_and_listed_identity,
            ),
            (
                "passing_a_result_row_keeps_its_badge",
                State::passing_a_result_row_keeps_its_badge,
            ),
            (
                "first_named_account_is_sent",
                State::first_named_account_is_sent,
            ),
        ];
        for (name, fixture) in fixtures {
            let result = fixture(state);
            println!("state-fixture name={name} ok={}", result.is_ok());
            if let Err(error) = result {
                self.fail(event_loop, error.context(name));
                return;
            }
        }
        let state = self.app.state.as_ref().expect("native State");
        let Some(false) = state.window.is_minimized() else {
            self.fail(
                event_loop,
                anyhow::anyhow!("not runnable here: visible native control was not confirmed"),
            );
            return;
        };
        let peer = PeerTasks::start(
            state.window.clone(),
            sender.expect("real App fan-in"),
            self.outgoing.take().expect("owned request receiver"),
            self.ledger.clone(),
            self.proxy.clone(),
        );
        match peer {
            Ok(peer) => {
                self.peer = Some(peer);
                self.begin(VISIBLE, false);
            }
            Err(error) => self.fail(event_loop, error),
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
        if let Err(error) = self.tick(event_loop) {
            self.fail(event_loop, error);
        }
    }
}

fn fixture_cli() -> crate::cli::Cli {
    crate::cli::Cli {
        dial: Vec::new(),
        socket: None,
        token: None,
        capture: None,
        scale: 1.0,
        start_mode: Some("files".into()),
        start_selected: None,
        auto_expand: false,
        demo_function_methods: None,
        start_maximized: false,
        start_fullscreen: false,
        start_path: None,
        demo_repl_eval: None,
        font_scale: Some(1.0),
        start_focus: "nav".into(),
        capture_preview: None,
        capture_delay_ms: 0,
        capture_cycle: 0,
        auto_pin: false,
        start_help: false,
        start_help_peek: false,
        start_monitor: false,
        start_scalebar: false,
        ephemeral: true,
        no_lease: true,
        demo_sessions: Vec::new(),
        demo_session_states: Vec::new(),
        demo_flash: Vec::new(),
        contrast_mode: "bright".into(),
        relaunched: false,
    }
}

pub(in crate::ui) fn minimized_window_drains_events_for_ten_minutes() -> Result<()> {
    let _home = FixtureHome::enter()?;
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
    let ledger = Arc::new(Mutex::new(NativeProgressLedger::default()));
    let _observation = observe_native_fan_in(ledger.clone());
    let event_loop = EventLoop::new().map_err(|e| anyhow::anyhow!("not runnable here: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (req_tx, req_rx) = tokio::sync::mpsc::unbounded_channel();
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
        ledger: ledger.clone(),
    };
    let mut driver = NativeDriver {
        app,
        ledger,
        proxy: event_loop.create_proxy(),
        outgoing: Some(req_rx),
        peer: None,
        current: None,
        next: None,
        transition: Instant::now(),
        outcome: None,
    };
    let run = crate::ui::init::with_native_startup(inputs, || event_loop.run_app(&mut driver));
    if let Some(peer) = driver.peer.take() {
        peer.stop()?;
    }
    run?;
    driver.outcome.unwrap_or_else(|| {
        Err(anyhow::anyhow!(
            "native proof exited without a completion verdict"
        ))
    })
}

struct FixtureHome {
    saved: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>,
    cwd: PathBuf,
    _state: crate::net::state::test_env::EnvGuard,
}

impl FixtureHome {
    fn enter() -> Result<Self> {
        let state = crate::net::state::test_env::set_test_env();
        let root = PathBuf::from(std::env::var_os("XDG_STATE_HOME").expect("owned fixture root"));
        let mut guard = Self {
            saved: Vec::new(),
            cwd: std::env::current_dir()?,
            _state: state,
        };
        for (name, value) in
            std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("SOT_"))
        {
            guard.saved.push((name.clone(), Some(value)));
            std::env::remove_var(name);
        }
        for name in [
            "HOME",
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "XDG_CONFIG_HOME",
            "XDG_RUNTIME_DIR",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
        ] {
            guard.saved.push((name.into(), std::env::var_os(name)));
            std::env::set_var(name, &root);
        }
        std::env::set_current_dir(&root)?;
        Ok(guard)
    }
}

impl Drop for FixtureHome {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.cwd).expect("restore fixture working directory");
        for (name, value) in self.saved.drain(..).rev() {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}
