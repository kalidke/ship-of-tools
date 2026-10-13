//! Native workload observations around the real App/State and transport loop.
//! The owned peer supplies events; only production callbacks drain them.

use super::native_children::{HarnessTick, NativeChild};
use super::*;
use crate::net::transport::{observe_native_fan_in, run_native_progress_transport, IncomingEvt};
use std::collections::{BTreeMap, BTreeSet};
use std::cell::Cell;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use winit::event_loop::EventLoopProxy;

const TEST: &str = "ui::app::tests::minimized_window_drains_events_for_ten_minutes";
pub(super) const HOST: &str = "<host>";
/// How long the workload child waits for the window's first three frames before its visible phase.
const FIRST_FRAMES: Duration = Duration::from_secs(20);

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
    accepted: BTreeSet<usize>,
    dequeued: BTreeSet<usize>,
    second_bins: Vec<usize>,
    max_lateness_ms: u128,
    final_dequeue: Option<Instant>,
    max_outstanding: usize,
    native_ok: bool,
    samples: usize,
    producer_done: Option<Instant>,
    timing_visits: Cell<usize>,
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
                        accepted: BTreeSet::new(),
                        dequeued: BTreeSet::new(),
                        second_bins: vec![0; profile.seconds as usize],
                        max_lateness_ms: 0,
                        final_dequeue: None,
                        max_outstanding: 0,
                        native_ok: true,
                        samples: 0,
                        producer_done: None,
                        timing_visits: Cell::new(0),
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
        self.accepted_at(tag, Instant::now());
    }

    fn accepted_at(&mut self, tag: Option<(String, usize)>, now: Instant) {
        let Some((phase, seq)) = tag else { return };
        let state = self
            .phases
            .get_mut(&phase)
            .expect("event has a started phase");
        assert!(
            (1..=state.profile.expected).contains(&seq),
            "workload sequence outside its profile"
        );
        assert!(
            state.accepted.insert(seq),
            "duplicate successful fan-in send"
        );
        let elapsed = now.duration_since(state.epoch);
        if let Some(bin) = state.second_bins.get_mut(elapsed.as_secs() as usize) {
            *bin += 1;
        }
        state.max_lateness_ms = state.max_lateness_ms.max(
            now.saturating_duration_since(state.epoch + Duration::from_millis(seq as u64 * 50)).as_millis());
        state.observe_outstanding(now);
    }

    pub(crate) fn dequeued(&mut self, host: &HostKey, event: &IncomingEvt) {
        let Some((phase, seq)) = self.tag(host, event) else {
            return;
        };
        self.dequeued_at(phase, seq, Instant::now());
    }

    fn dequeued_at(&mut self, phase: String, seq: usize, now: Instant) {
        let state = self
            .phases
            .get_mut(&phase)
            .expect("dequeue has a started phase");
        assert!(
            state.accepted.contains(&seq),
            "a dequeue preceded accepted-send accounting"
        );
        assert!(
            state.dequeued.insert(seq),
            "duplicate actual State dequeue"
        );
        state.final_dequeue = Some(state.final_dequeue.map_or(now, |last| last.max(now)));
        state.observe_outstanding(now);
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
        let seconds = elapsed.as_secs().min(self.profile.seconds as u64) as usize;
        if seconds < 10 {
            return (0, 0, self.max_lateness_ms);
        }
        let mut sum: usize = self.second_bins[..10].iter().map(|&bin| {
            self.timing_visits.set(self.timing_visits.get() + 1);
            bin
        }).sum();
        let (mut min, mut max) = (sum, sum);
        for end in 10..seconds {
            self.timing_visits.set(self.timing_visits.get() + 2);
            sum += self.second_bins[end];
            sum -= self.second_bins[end - 10];
            min = min.min(sum);
            max = max.max(sum);
        }
        (min, max, self.max_lateness_ms)
    }

    fn snapshot(&self, now: Instant, native: bool) -> ProgressSnapshot {
        ProgressSnapshot {
            profile: self.profile,
            elapsed: now.duration_since(self.epoch),
            accepted: self.accepted.len(),
            dequeued: self.dequeued.len(),
            max_outstanding: self.max_outstanding,
            native_ok: self.native_ok,
            samples: self.samples,
            producer_done: self.producer_done,
            final_dequeue: self.final_dequeue,
            rate: self.rate(now.duration_since(self.epoch)),
            native,
        }
    }
}

/// All observations used by output and verdicts, owned independently of the ledger.
struct ProgressSnapshot {
    profile: Profile,
    elapsed: Duration,
    accepted: usize,
    dequeued: usize,
    max_outstanding: usize,
    native_ok: bool,
    samples: usize,
    producer_done: Option<Instant>,
    final_dequeue: Option<Instant>,
    rate: (usize, usize, u128),
    native: bool,
}

impl ProgressSnapshot {
    /// The workload kept 20 sends a second: every ten-second window holds 190 to 210 of them, so a stall that moves 11
    /// sends past a second's edge fails (any stall over about 1.55 s does). A minimized window needs no tighter bound:
    /// it pings every 30 s and the daemon reaps a connection silent for 90 s, so it keeps its connection through any
    /// stall under 60 s. In the visible phase no send may be over 250 ms late either: that is how a transport waiting
    /// on the held window thread shows (MAC-REDRAW).
    fn rate_ok(&self) -> bool {
        let (min, max, late) = self.rate;
        (self.profile.minimized || late <= 250) && (self.profile.seconds < 10 || (min >= 190 && max <= 210))
    }

    fn failures(&self) -> Vec<&'static str> {
        let mut failures = Vec::new();
        if self.accepted != self.profile.expected {
            failures.push("workload_volume");
        }
        if !self.rate_ok() {
            failures.push("workload_rate");
        }
        if self.dequeued != self.accepted
            || self.max_outstanding > 200
        {
            failures.push("queue_progress");
        }
        if self.samples < self.profile.seconds as usize
            || !self.native_ok
            || self.elapsed.as_secs() < self.profile.seconds as u64
        {
            failures.push("native_observation");
        }
        if self.producer_done.is_none()
            || self.final_dequeue.is_some_and(|at| at > self.producer_done.unwrap() + Duration::from_secs(5))
        {
            failures.push("final_drain");
        }
        failures
    }

    fn write(&self, phase: &str, writer: &mut impl Write) -> std::io::Result<()> {
        let (min, max, late) = self.rate;
        let workload_ok = self.accepted == self.profile.expected && self.rate_ok();
        writeln!(writer, "window-progress test={TEST} phase={phase} elapsed_s={} expected_sent={} sent={} dequeued={} outstanding={} max_outstanding={} min_10s_sent={min} max_10s_sent={max} max_lateness_ms={late} native_minimized={} workload_ok={workload_ok} samples={}",
            self.elapsed.as_secs(), self.profile.expected, self.accepted, self.dequeued, self.accepted - self.dequeued, self.max_outstanding, self.native, self.samples)?;
        writer.flush()
    }
}

enum ReportRequest {
    Begin(Profile, bool),
    Sample(Profile, Instant, bool, usize),
}

struct ProgressReport {
    epoch: Instant,
    snapshot: ProgressSnapshot,
    completed: bool,
    failures: Vec<&'static str>,
}

impl ProgressReport {
    fn write(&self, body_entered: bool, mutation: bool, writer: &mut impl Write) -> std::io::Result<()> {
        self.snapshot.write(if body_entered { "body-entered" } else { "sample" }, writer)?;
        if !self.completed {
            return Ok(());
        }
        self.snapshot.write("completed", writer)?;
        let profile = self.snapshot.profile;
        if mutation && profile.phase == VISIBLE.phase {
            writeln!(writer, "mac-redraw-mutation visible failures={}", self.failures.join(","))?;
        } else if profile.stall && self.failures.contains(&"workload_volume")
            && self.failures.contains(&"workload_rate") && !self.failures.contains(&"native_observation")
        {
            writeln!(writer, "window-progress control=stalled_minimized_producer_is_rejected expected_failure=workload_volume,workload_rate observed_failure={} control_bodies=1", self.failures.join(","))?;
        } else if profile.phase == MINIMIZED.phase && self.failures.is_empty() {
            writeln!(writer, "window-progress phase=completed expected_sent=12000 sent=12000 dequeued=12000 outstanding=0 workload_ok=true completed_bodies=1 samples={}", self.snapshot.samples)?;
        }
        writer.flush()
    }
}

fn report_progress(
    ledger: &Mutex<NativeProgressLedger>,
    request: ReportRequest,
    mutation: bool,
    writer: &mut impl Write,
) -> std::io::Result<ProgressReport> {
    let mut ledger = ledger.lock().expect("native progress ledger");
    let body_entered = matches!(request, ReportRequest::Begin(..));
    let (profile, now, native, count) = match request {
        ReportRequest::Begin(profile, native) => (profile, ledger.begin(profile), native, 0),
        ReportRequest::Sample(profile, now, native, count) => (profile, now, native, count),
    };
    let phase = ledger.phases.get_mut(profile.phase).expect("started current phase");
    if !body_entered {
        phase.native_ok &= native == profile.minimized;
        phase.samples += count;
        phase.observe_outstanding(now);
    }
    let completed = !body_entered && phase.producer_done.is_some_and(|done| {
        phase.outstanding() == 0 || now.duration_since(done) >= Duration::from_secs(5)
    });
    let snapshot = phase.snapshot(now, native);
    let failures = if completed { snapshot.failures() } else { Vec::new() };
    let report = ProgressReport { epoch: phase.epoch, snapshot, completed, failures };
    drop(ledger);
    report.write(body_entered, mutation, writer)?;
    Ok(report)
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
                if state.accepted.contains(&profile.expected) {
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

/// The mutation leg (`test-mac-redraw-mutation`): the harness transport wakes the window by a `Window` call from its own
/// thread, as every transport did before `MainWake`.
#[cfg(feature = "test-mac-redraw-mutation")]
struct OffMainRedraw(Arc<Window>);

#[cfg(feature = "test-mac-redraw-mutation")]
impl crate::net::transport::Redraw for OffMainRedraw {
    fn request_redraw(&self) {
        self.0.request_redraw();
    }
}

struct PeerTasks {
    rt: tokio::runtime::Runtime,
    profiles: Option<tokio::sync::mpsc::UnboundedSender<(Profile, Instant)>>,
    tasks: Vec<tokio::task::JoinHandle<Result<()>>>,
}

impl PeerTasks {
    fn start(
        wake: impl crate::net::transport::Redraw + Send + Sync + 'static,
        sender: std::sync::mpsc::Sender<(HostKey, IncomingEvt)>,
        mut outgoing: tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        ledger: Arc<Mutex<NativeProgressLedger>>,
        proxy: EventLoopProxy<()>,
        harness_tick: HarnessTick,
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
                &wake,
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
                harness_tick.post();
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
    /// The App's fan-in sender, cloned before `resumed`, which takes the App's own.
    fan_in: Option<std::sync::mpsc::Sender<(HostKey, IncomingEvt)>>,
    ledger: Arc<Mutex<NativeProgressLedger>>,
    proxy: EventLoopProxy<()>,
    harness_tick: HarnessTick,
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

    /// Starts the owned peer and the visible phase once the window has drawn three frames, so the cost of its first
    /// frames (about 0.9 s on a hosted macOS runner) falls outside the measured workload.
    fn start_when_drawn(&mut self, event_loop: &ActiveEventLoop) {
        if self.peer.is_some() || self.outcome.is_some() {
            return;
        }
        let Some(state) = self.app.state.as_ref() else { return };
        if state.frame_counter < 3 {
            if self.transition.elapsed() > FIRST_FRAMES {
                let drawn = state.frame_counter;
                self.fail(event_loop, anyhow::anyhow!("the window drew {drawn} frames in {FIRST_FRAMES:?}; the workload starts after 3"));
            }
            return;
        }
        let Some(false) = state.window.is_minimized() else {
            self.fail(event_loop, anyhow::anyhow!("not runnable here: visible native control was not confirmed"));
            return;
        };
        #[cfg(not(feature = "test-mac-redraw-mutation"))]
        let wake = state.wake.clone();
        #[cfg(feature = "test-mac-redraw-mutation")]
        let wake = OffMainRedraw(state.window.off_main_for_the_mutation_leg());
        let peer = PeerTasks::start(
            wake,
            self.fan_in.take().expect("real App fan-in"),
            self.outgoing.take().expect("owned request receiver"),
            self.ledger.clone(),
            self.proxy.clone(),
            self.harness_tick.clone(),
        );
        match peer {
            Ok(peer) => {
                self.peer = Some(peer);
                self.begin(VISIBLE, false);
                // The visible phase starts with the window's thread held, as a slow frame holds it. The transport must
                // keep reading meanwhile: it wakes the window through `MainWake`, never a `Window` call, which on
                // macOS would wait here. So the phase's 250 ms lateness bound still holds.
                const MAIN_HOLD: Duration = Duration::from_millis(600);
                std::thread::sleep(MAIN_HOLD);
            }
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn begin(&mut self, profile: Profile, native: bool) {
        let report = report_progress(&self.ledger, ReportRequest::Begin(profile, native), false, &mut std::io::stdout())
            .expect("write native progress proof");
        let epoch = report.epoch;
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

    fn tick(&mut self, event_loop: &ActiveEventLoop, count: usize) -> Result<()> {
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
        let report = report_progress(&self.ledger, ReportRequest::Sample(profile, now, native, count),
            cfg!(feature = "test-mac-redraw-mutation"), &mut std::io::stdout())?;
        if !report.completed {
            return Ok(());
        }
        let failures = report.failures;
        // The mutation leg ends at the visible phase's verdict. Its transport may be waiting on this thread (the defect
        // it restores), so there is no orderly stop: it reports and ends the process.
        #[cfg(feature = "test-mac-redraw-mutation")]
        if profile.phase == VISIBLE.phase {
            std::process::exit(i32::from(!failures.is_empty()));
        }
        if profile.stall {
            anyhow::ensure!(
                failures.contains(&"workload_volume")
                    && failures.contains(&"workload_rate")
                    && !failures.contains(&"native_observation"),
                "stalled-producer control did not settle its two workload predicates: {failures:?}"
            );
        } else {
            anyhow::ensure!(
                failures.is_empty(),
                "native {} proof failed: {}",
                profile.phase,
                failures.join(",")
            );
        }
        if profile.phase == MINIMIZED.phase {
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
        if self.fan_in.is_none() {
            self.fan_in = self.app.evt_tx.clone();
        }
        let building = self.app.state.is_none();
        self.app.resumed(event_loop);
        if self.app.state.is_none() && self.outcome.is_none() {
            self.fail(
                event_loop,
                anyhow::anyhow!("not runnable here: native App/State startup did not complete"),
            );
        } else if building {
            // The first-frames wait counts from the window's existence: building it is startup, which only WORKLOAD_BOUND bounds.
            self.transition = Instant::now();
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
        self.start_when_drawn(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, (): ()) {
        let tick = self.harness_tick.clone();
        tick.dispatch(self, |driver| driver.app.user_event(event_loop, ()), |driver, count| {
            if driver.outcome.is_some() {
                return;
            }
            if let Err(error) = driver.tick(event_loop, count) {
                driver.fail(event_loop, error);
            }
        });
    }
}

pub(super) fn fixture_cli() -> crate::cli::Cli {
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

/// The workload child: the visible, stalled-control and minimized phases on a State that nothing else has touched.
pub(in crate::ui) fn minimized_window_drains_events_for_ten_minutes() -> Result<()> {
    let ledger = Arc::new(Mutex::new(NativeProgressLedger::default()));
    let _observation = observe_native_fan_in(ledger.clone());
    let NativeChild { _home, event_loop, app, outgoing, inputs } = super::native_children::native_child(ledger.clone())?;
    let mut driver = NativeDriver {
        app,
        fan_in: None,
        ledger,
        proxy: event_loop.create_proxy(),
        harness_tick: HarnessTick::default(),
        outgoing: Some(outgoing),
        peer: None,
        current: None,
        next: None,
        transition: Instant::now(),
        outcome: None,
    };
    let run = crate::ui::init::with_native_startup(inputs, || event_loop.run_app(&mut driver));
    // The verdict first: a peer that cannot stop in its bound must not hide what the workload proved or failed.
    let verdict = run.map_err(anyhow::Error::from).and_then(|()| {
        driver.outcome.take().unwrap_or_else(|| {
            Err(anyhow::anyhow!(
                "native proof exited without a completion verdict"
            ))
        })
    });
    let stopped = driver.peer.take().map_or(Ok(()), PeerTasks::stop);
    verdict.and(stopped)
}

pub(super) struct FixtureHome {
    saved: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>,
    cwd: PathBuf,
    _state: crate::net::state::test_env::EnvGuard,
}

impl FixtureHome {
    pub(super) fn enter() -> Result<Self> {
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

#[allow(dead_code, reason = "harness=false native targets run none of these tests")]
#[path = "native_harness_tests.rs"]
mod harness_tests;
