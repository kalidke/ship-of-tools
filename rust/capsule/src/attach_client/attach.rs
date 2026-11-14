//! The attach worker: one thread that keeps a viewer attached to a row's socket.
//!
//! It waits until the row is viewed and the endpoint's link is up, dials once, attaches (a driver for a window, a
//! watcher for the wake), hands the snapshot and then the output to its sink, and carries the caller's input and
//! resizes to the row. A lost connection is redialed on platform's `Redial`; some failures end the episode for good.
//! The worker holds no screen: the sink's owner restores the snapshot into its parser.

use crate::endpoint::Endpoint;
use sot_platform::transport::TransportError;
use crate::row::{RowClient, RowError, RowEvent, RowWaker, NOT_DRIVER};
use sot_platform::host::redial::Redial;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The most input held while the row is not attached, in bytes; more is dropped and counted.
pub const HELD_INPUT_CAP: usize = 8192;
/// The most input queued for a row that is attached, in bytes: a paste fits, a stalled row does not grow without end.
const QUEUED_INPUT_CAP: usize = 4 * 1024 * 1024;
/// Output bytes the sink may hold. Past it, a client on screen stops reading the row, which then evicts a viewer that
/// falls behind and shows it a fresh snapshot on its redial; a client off screen keeps reading and drops the output, so
/// the row never cuts it for not being read, and asks for the screen as it is when it is shown again.
const SINK_BOUND_BYTES: usize = 4 * 1024 * 1024;
/// The first wait and the cap of a redial: 250 ms doubling to 4 s, started over only after a connection that lasted
/// `STABLE`.
pub const REDIAL_FLOOR: Duration = Duration::from_millis(250);
pub const REDIAL_CAP: Duration = Duration::from_secs(4);
/// How long a row may stay absent (its socket not there, though the daemon answers), attempt after attempt, before the
/// episode ends. A daemon that cannot be reached says nothing about the row, so such an attempt starts the window over.
pub const ABSENT_WINDOW: Duration = Duration::from_secs(120);
/// How long the row's hello may take once the stream is up, and the row's screen after it.
const HELLO_BOUND: Duration = Duration::from_secs(10);
/// How often an idle worker looks at its flags; a connected one wakes on the row's frames and on its caller.
const IDLE_TICK: Duration = Duration::from_millis(20);
/// How long a connected worker waits for a frame before it looks at its caller's commands.
const EVENT_TICK: Duration = Duration::from_millis(250);

/// What the worker tells its sink.
#[derive(Debug)]
pub enum WorkerEvent {
    /// The row's screen at attach or resync, whole, and the run it shows.
    Checkpoint { bytes: Vec<u8>, run: String },
    Output(Vec<u8>),
    Status(String),
    /// The episode has ended for good; the text says why.
    Terminal(String),
}

/// The row's answer to one input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome {
    /// The bytes reached the terminal.
    Recorded,
    /// The pen was no longer this client's, and the client did not take it back: a headless client never does, and a
    /// window does once per frame (the pen was taken again as it did, or the row refused it).
    RefusedStale,
    /// The row may or may not have written the bytes; they are never sent again.
    DeliveryUnknown,
    /// The input was typed into a run that has ended; it was not written into the one that replaced it.
    RunEnded,
}

enum Command {
    /// Bytes, the run they were typed into (`None` before the first attach), and the run they are meant for, if one was
    /// named: the row is told it, and refuses them in any other.
    Input(Vec<u8>, Option<String>, Option<String>),
    Resize(u16, u16),
}

/// State the caller and the worker share.
struct Shared {
    viewed: AtomicBool,
    shutdown: AtomicBool,
    shutdown_reason: Mutex<&'static str>,
    host: String,
    attached: AtomicBool,
    /// Bytes of input in the command queue.
    queued_input: AtomicUsize,
    discarded: AtomicUsize,
    /// Output bytes sent to the sink and not yet consumed by it.
    sink_bytes: AtomicUsize,
    waker: Mutex<Option<RowWaker>>,
    recorded: AtomicU64,
    outcome: Mutex<Option<InputOutcome>>,
    run: Mutex<String>,
    size: Mutex<(u16, u16)>,
    /// The run in which a headless client has taken the pen: its transaction is that run's alone.
    taken_run: Mutex<Option<String>>,
}

impl Shared {
    fn new(cols: u16, rows: u16, viewed: bool) -> Self {
        Self {
            viewed: AtomicBool::new(viewed),
            shutdown: AtomicBool::new(false),
            shutdown_reason: Mutex::new("drop"),
            host: String::new(),
            attached: AtomicBool::new(false),
            queued_input: AtomicUsize::new(0),
            discarded: AtomicUsize::new(0),
            sink_bytes: AtomicUsize::new(0),
            waker: Mutex::new(None),
            recorded: AtomicU64::new(0),
            outcome: Mutex::new(None),
            run: Mutex::new(String::new()),
            size: Mutex::new((cols, rows)),
            taken_run: Mutex::new(None),
        }
    }

    fn current_run(&self) -> String {
        self.run.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn wake(&self) {
        if let Some(w) = self.waker.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            w.wake();
        }
    }
}

/// The caller's end of a running worker.
pub struct AttachWorker {
    shared: Arc<Shared>,
    commands: Sender<Command>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl AttachWorker {
    /// Starts the worker. `row` is the row in `endpoint`'s own namespace; `headless` means a watcher that takes the pen
    /// only to type, once, and never resizes.
    pub fn spawn<E, S>(endpoint: E, row: String, cols: u16, rows: u16, headless: bool, sink: S) -> std::io::Result<Self>
    where
        E: Endpoint + Send + 'static,
        E::Client: 'static,
        S: Fn(WorkerEvent) + Send + 'static,
    {
        let host = sot_platform::host::state_dir::host_name().unwrap_or_else(|e| format!("local host unavailable: {e}"));
        Self::spawn_on_host(endpoint, host, row, cols, rows, headless, sink)
    }

    /// As [`Self::spawn`], with the dial host's name for the window's diagnostics.
    pub fn spawn_on_host<E, S>(endpoint: E, host: String, row: String, cols: u16, rows: u16, headless: bool, sink: S) -> std::io::Result<Self>
    where
        E: Endpoint + Send + 'static,
        E::Client: 'static,
        S: Fn(WorkerEvent) + Send + 'static,
    {
        let mut state = Shared::new(cols, rows, true);
        state.host = host;
        let shared = Arc::new(state);
        let (commands, inbox) = mpsc::channel();
        let handle = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new().name("row-attach".into()).spawn(move || Worker { endpoint, row, headless, shared, inbox, sink }.run())?
        };
        Ok(Self { shared, commands, handle: Mutex::new(Some(handle)) })
    }

    /// A worker with no thread, for a test that feeds the client's events by hand.
    #[cfg(test)]
    pub(crate) fn inert() -> Self {
        let (commands, _inbox) = mpsc::channel();
        let shared = Arc::new(Shared::new(80, 24, false));
        Self { shared, commands, handle: Mutex::new(None) }
    }

    /// A client off screen starts no dial; it finishes the attempt in flight and waits until it is viewed again. Shown
    /// again, the worker is woken, so the screen it asks for does not wait out a quiet row's read tick.
    pub fn set_viewed(&self, viewed: bool) {
        if !self.shared.viewed.swap(viewed, Ordering::AcqRel) && viewed {
            self.shared.wake();
        }
    }

    /// Queues input for the row. Before the attach only [`HELD_INPUT_CAP`] bytes are held; the rest are dropped and
    /// counted. Returns whether the bytes were kept.
    pub fn send_input(&self, bytes: Vec<u8>) -> bool {
        self.send_input_for(bytes, None)
    }

    /// [`Self::send_input`] meant for the run `run` names: never written into another, however the worker has been
    /// reconnected since, and the row refuses it in any run but that one.
    pub fn send_input_for(&self, bytes: Vec<u8>, run: Option<String>) -> bool {
        let cap = if self.shared.attached.load(Ordering::Acquire) { QUEUED_INPUT_CAP } else { HELD_INPUT_CAP };
        let queued = self.shared.queued_input.load(Ordering::Acquire);
        if queued.saturating_add(bytes.len()) > cap {
            self.shared.discarded.fetch_add(1, Ordering::AcqRel);
            return false;
        }
        self.shared.queued_input.fetch_add(bytes.len(), Ordering::AcqRel);
        // The answer to the most recent input: none until this one is answered.
        *self.shared.outcome.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let typed_into = run.clone().or_else(|| Some(self.shared.current_run()).filter(|r| !r.is_empty()));
        if self.commands.send(Command::Input(bytes, typed_into, run)).is_err() {
            return false;
        }
        self.shared.wake();
        true
    }

    /// The size the pane wants; applied to the row when this client holds the pen.
    pub fn resize(&self, cols: u16, rows: u16) {
        *self.shared.size.lock().unwrap_or_else(|p| p.into_inner()) = (cols, rows);
        if self.commands.send(Command::Resize(cols, rows)).is_ok() {
            self.shared.wake();
        }
    }

    pub fn inputs_discarded(&self) -> usize {
        self.shared.discarded.load(Ordering::Acquire)
    }

    pub fn recorded_bytes(&self) -> u64 {
        self.shared.recorded.load(Ordering::Acquire)
    }

    /// The row's answer to the most recent input, `None` while it is unanswered.
    pub fn last_input_outcome(&self) -> Option<InputOutcome> {
        *self.shared.outcome.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The run the row declared at the last attach; empty before the first.
    pub fn run(&self) -> String {
        self.shared.run.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The sink's owner took `bytes` of output; the worker may read that much more.
    pub fn ack_output_consumed(&self, bytes: usize) {
        let _ = self.shared.sink_bytes.fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| Some(held.saturating_sub(bytes)));
    }

    /// Names a caller's intentional retirement without changing when the worker ends.
    pub fn set_shutdown_reason(&self, reason: &'static str) {
        *self.shared.shutdown_reason.lock().unwrap_or_else(|p| p.into_inner()) = reason;
    }

    /// Ends the worker and waits up to `wait` for its thread; `true` when it ended.
    pub fn shutdown(&self, wait: Duration) -> bool {
        {
            let mut reason = self.shared.shutdown_reason.lock().unwrap_or_else(|p| p.into_inner());
            if *reason == "drop" { *reason = "shutdown"; }
        }
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake();
        let Some(handle) = self.handle.lock().unwrap_or_else(|p| p.into_inner()).take() else { return true };
        let deadline = Instant::now() + wait;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                tracing::warn!("fe attach worker did not end within {wait:?}");
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = handle.join();
        true
    }
}

impl Drop for AttachWorker {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake();
    }
}

struct Worker<E: Endpoint, S> {
    endpoint: E,
    row: String,
    headless: bool,
    shared: Arc<Shared>,
    inbox: Receiver<Command>,
    sink: S,
}

/// How one connection ended.
enum Ended {
    Shutdown,
    Lost(RowError),
}

struct AttemptLog {
    started: Instant,
    stage: &'static str,
    error: String,
}

impl AttemptLog {
    fn row_error(&mut self, e: RowError) -> Attempt {
        self.error = e.diagnostic();
        row_error(e)
    }
}

/// How one attempt to connect and attach ended before it served.
enum Attempt {
    /// Try again; `absent` is whether the row's socket was not there (the window's clock), as against trouble reaching it.
    Retry { reason: String, absent: bool },
    Terminal(String),
}

impl<E, S> Worker<E, S>
where
    E: Endpoint,
    E::Client: 'static,
    S: Fn(WorkerEvent),
{
    fn stopping(&self) -> bool {
        self.shared.shutdown.load(Ordering::Acquire)
    }

    fn run(self) {
        let mut redial = Redial::new(REDIAL_FLOOR, REDIAL_CAP);
        let mut absent_since: Option<Instant> = None;
        static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);
        let client_id = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
        let mut attempt = 0u64;
        loop {
            if !self.wait_until_ready() {
                return;
            }
            attempt += 1;
            let mut log = AttemptLog { started: Instant::now(), stage: "connecting", error: String::new() };
            let result = self.attempt(&mut log);
            let result = match result {
                Ok((mut client, driver)) => {
                    log.stage = "service";
                    let ended = self.serve(&mut client, driver);
                    if let Ended::Lost(e) = &ended { log.error = e.diagnostic(); }
                    self.shared.attached.store(false, Ordering::Release);
                    *self.shared.waker.lock().unwrap_or_else(|p| p.into_inner()) = None;
                    Ok(ended)
                }
                Err(e) => Err(e),
            };
            let shutdown = self.stopping() || matches!(result, Ok(Ended::Shutdown));
            let cause = if shutdown { *self.shared.shutdown_reason.lock().unwrap_or_else(|p| p.into_inner()) } else { "connection lost" };
            // A headless client lives for one transaction, several a minute per row: only its loss is news.
            if !(self.headless && shutdown) {
                tracing::info!(
                    host = ?self.shared.host, row = ?self.row, client = client_id, attempt, stage = %log.stage,
                    end = %if shutdown { "shutdown" } else { "lost" }, cause = ?cause,
                    elapsed_since_attempt_start_ms = log.started.elapsed().as_millis() as u64,
                    error = ?log.error, "row attach ended"
                );
            }
            let lasted = match result {
                Ok(ended) => {
                    absent_since = None;
                    if matches!(ended, Ended::Shutdown) {
                        return;
                    }
                    (self.sink)(WorkerEvent::Status("redialing\u{2026}".into()));
                    log.started.elapsed()
                }
                Err(Attempt::Terminal(reason)) => {
                    (self.sink)(WorkerEvent::Terminal(reason));
                    return;
                }
                Err(Attempt::Retry { reason, absent }) => {
                    if absent {
                        let since = *absent_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= ABSENT_WINDOW {
                            (self.sink)(WorkerEvent::Terminal(format!("the row did not answer for {ABSENT_WINDOW:?}: {reason}")));
                            return;
                        }
                    } else {
                        absent_since = None;
                    }
                    (self.sink)(WorkerEvent::Status(format!("redialing\u{2026} ({reason})")));
                    Duration::ZERO
                }
            };
            let wait = redial.after(lasted);
            let until = Instant::now() + wait;
            while Instant::now() < until {
                if self.stopping() {
                    return;
                }
                std::thread::sleep(IDLE_TICK.min(until.saturating_duration_since(Instant::now())));
            }
        }
    }

    /// Waits for the row to be viewed and its link up; `false` when the worker was told to end.
    fn wait_until_ready(&self) -> bool {
        loop {
            if self.stopping() {
                return false;
            }
            if self.shared.viewed.load(Ordering::Acquire) && self.endpoint.link_up() {
                return true;
            }
            std::thread::sleep(IDLE_TICK);
        }
    }

    /// Dials, proves the row, attaches and sends the snapshot to the sink.
    fn attempt(&self, log: &mut AttemptLog) -> Result<(RowClient<E::Client>, bool), Attempt> {
        // The login (an ssh one has a budget of its own, topology's) is the endpoint's to bound; the row's hello has its
        // own, which starts when the stream is up.
        let conn = self.endpoint.connect_row_unchallenged(&self.row).map_err(|e| {
            log.error = format!("{e:#}");
            dial_error(e)
        })?;
        log.stage = "hello";
        let deadline = Instant::now() + HELLO_BOUND;
        let mut client = RowClient::over(&self.endpoint, conn, "fe-attach", deadline).map_err(|e| log.row_error(e))?;
        log.stage = "attach";
        let (cols, rows) = *self.shared.size.lock().unwrap_or_else(|p| p.into_inner());
        let mut driver = !self.headless;
        let run = client.attach(cols, rows, driver).map_err(|e| log.row_error(e))?;
        log.stage = "snapshot";
        *self.shared.run.lock().unwrap_or_else(|p| p.into_inner()) = run;
        *self.shared.waker.lock().unwrap_or_else(|p| p.into_inner()) = Some(client.waker());
        // The snapshot comes before the first output, in chunks the last of which is flagged.
        let mut snapshot = Vec::new();
        let deadline = Instant::now() + HELLO_BOUND;
        loop {
            if self.stopping() {
                return Err(Attempt::Retry { reason: "ending".into(), absent: false });
            }
            match client.next_event(EVENT_TICK).map_err(|e| log.row_error(e))? {
                Some(RowEvent::Snapshot { bytes, last }) => {
                    snapshot.extend_from_slice(&bytes);
                    if last {
                        break;
                    }
                }
                // Another connection took the pen while the screen was on its way: this client is a watcher, and its next
                // key takes the pen again.
                Some(RowEvent::Demoted) => driver = false,
                // An attach is no watch: the row sends it neither of the watch's reports.
                Some(RowEvent::Output(_) | RowEvent::Phase(_) | RowEvent::Exiting(_)) => {}
                None => {
                    if Instant::now() >= deadline {
                        log.error = "the row sent no screen".into();
                        return Err(Attempt::Retry { reason: "the row sent no screen".into(), absent: false });
                    }
                }
            }
        }
        (self.sink)(WorkerEvent::Checkpoint { bytes: snapshot, run: self.shared.current_run() });
        (self.sink)(WorkerEvent::Status("attached".into()));
        self.shared.attached.store(true, Ordering::Release);
        Ok((client, driver))
    }

    /// Follows the row until the connection ends or the worker is told to.
    fn serve(&self, client: &mut RowClient<E::Client>, mut driver: bool) -> Ended {
        // A viewer the row resynced (it fell behind, and its queued output was dropped) is sent a fresh screen at the next
        // ground state: the chunks are gathered here and handed over whole.
        let mut resync: Vec<u8> = Vec::new();
        // Output dropped while this client was off screen: the screen its owner holds is behind the row's.
        let mut behind = false;
        loop {
            if self.stopping() {
                return Ended::Shutdown;
            }
            let viewed = self.shared.viewed.load(Ordering::Acquire);
            let full = self.shared.sink_bytes.load(Ordering::Acquire) > SINK_BOUND_BYTES;
            // The sink's owner has not consumed what it was given. On screen: stop reading, so the row's own queue fills
            // and it evicts a viewer that cannot keep up. Off screen: keep reading and drop the output, since a row
            // whose writes stall for its write bound cuts the connection, and the window would learn of that only
            // once it had replayed the stale output of a client it already shows as attached.
            if full && viewed {
                std::thread::sleep(Duration::from_millis(2));
            } else {
                match client.next_event(EVENT_TICK) {
                    Ok(Some(RowEvent::Output(_))) if full => behind = true,
                    Ok(Some(RowEvent::Output(bytes))) => {
                        self.shared.sink_bytes.fetch_add(bytes.len(), Ordering::AcqRel);
                        (self.sink)(WorkerEvent::Output(bytes));
                    }
                    Ok(Some(RowEvent::Demoted)) => driver = false,
                    Ok(Some(RowEvent::Snapshot { bytes, last })) => {
                        resync.extend_from_slice(&bytes);
                        if last {
                            (self.sink)(WorkerEvent::Checkpoint {
                                bytes: std::mem::take(&mut resync), run: self.shared.current_run(),
                            });
                        }
                    }
                    Ok(None | Some(RowEvent::Phase(_) | RowEvent::Exiting(_))) => {}
                    Err(e) => return Ended::Lost(e),
                }
            }
            // Output dropped while a request waited, or while off screen: ask the row for the screen as it is, which
            // arrives as a snapshot. Whether it is on screen is read afresh: it may have been shown during the read above.
            if client.take_lost_output() || (behind && self.shared.viewed.load(Ordering::Acquire)) {
                behind = false;
                let (cols, rows) = *self.shared.size.lock().unwrap_or_else(|p| p.into_inner());
                match client.attach(cols, rows, driver) {
                    Ok(run) => *self.shared.run.lock().unwrap_or_else(|p| p.into_inner()) = run,
                    Err(e) => return Ended::Lost(e),
                }
            }
            while let Ok(command) = self.inbox.try_recv() {
                match command {
                    Command::Input(bytes, typed_into, meant_for) => {
                        self.shared.queued_input.fetch_sub(bytes.len(), Ordering::AcqRel);
                        // Typed into a run that is not this one (the connection was lost, and the row's agent started
                        // again): never written here.
                        if typed_into.is_some_and(|run| run != self.shared.current_run()) {
                            self.shared.discarded.fetch_add(1, Ordering::AcqRel);
                            self.settle(InputOutcome::RunEnded);
                            continue;
                        }
                        if let Err(ended) = self.type_input(client, &bytes, meant_for.as_deref(), &mut driver, false) {
                            return ended;
                        }
                    }
                    Command::Resize(cols, rows) => {
                        if driver && !self.headless && client.resize(cols, rows).is_err() {
                            tracing::debug!("the row refused a resize");
                        }
                    }
                }
            }
        }
    }

    /// Types `bytes`, taking the pen first when this client does not hold it. A window's bytes the row refuses because
    /// another client took the pen (nothing was written, and the window had not yet read its demotion) are sent once
    /// more after the pen is taken back (`retaken` marks that send), before the next frame; a headless client's never are.
    fn type_input(
        &self, client: &mut RowClient<E::Client>, bytes: &[u8], meant_for: Option<&str>, driver: &mut bool, retaken: bool,
    ) -> Result<(), Ended> {
        if !*driver {
            // A watcher that typed once in a run gives up the pen to whoever took it after, and does not fight for it,
            // not even over a new connection to the same run; in another run it has not typed.
            let run = self.shared.current_run();
            if self.headless && self.shared.taken_run.lock().unwrap_or_else(|p| p.into_inner()).as_deref() == Some(run.as_str()) {
                self.settle(InputOutcome::RefusedStale);
                return Ok(());
            }
            match client.take() {
                Ok(()) => {
                    *driver = true;
                    *self.shared.taken_run.lock().unwrap_or_else(|p| p.into_inner()) = Some(run);
                    if !self.headless {
                        let (cols, rows) = *self.shared.size.lock().unwrap_or_else(|p| p.into_inner());
                        let _ = client.resize(cols, rows);
                    }
                }
                Err(RowError::Refused(_)) => {
                    self.settle(InputOutcome::RefusedStale);
                    return Ok(());
                }
                Err(e) => {
                    self.settle(InputOutcome::DeliveryUnknown);
                    return Err(Ended::Lost(e));
                }
            }
        }
        match client.send_input_in(bytes, meant_for) {
            Ok(written) => {
                self.shared.recorded.fetch_add(written as u64, Ordering::AcqRel);
                self.settle(InputOutcome::Recorded);
                Ok(())
            }
            Err(RowError::Refused(msg)) if msg.starts_with("run ended") => {
                self.shared.discarded.fetch_add(1, Ordering::AcqRel);
                self.settle(InputOutcome::RunEnded);
                Ok(())
            }
            Err(RowError::Refused(msg)) if msg == NOT_DRIVER && !self.headless && !retaken => {
                *driver = false;
                self.type_input(client, bytes, meant_for, driver, true)
            }
            Err(RowError::Refused(_)) => {
                *driver = false;
                self.settle(InputOutcome::RefusedStale);
                Ok(())
            }
            Err(RowError::DeliveryUnknown) => {
                self.settle(InputOutcome::DeliveryUnknown);
                Ok(())
            }
            Err(e) => {
                self.settle(InputOutcome::DeliveryUnknown);
                Err(Ended::Lost(e))
            }
        }
    }

    fn settle(&self, outcome: InputOutcome) {
        *self.shared.outcome.lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome);
    }
}

/// A failed dial: some are final, the rest are tried again.
fn dial_error(e: TransportError) -> Attempt {
    match e {
        // A path that can never fit is the same on every try.
        TransportError::PathTooLong(path) => Attempt::Terminal(format!("the row socket path is too long: {}", path.display())),
        TransportError::Refused { code, detail } => Attempt::Terminal(format!("the daemon refused the row ({code}): {detail}")),
        TransportError::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => {
            Attempt::Terminal(format!("access to the row was denied: {source}"))
        }
        // The daemon (or the ssh login to it) did not answer: said in those words, since a person reads it.
        TransportError::Unreachable(e) => Attempt::Retry { reason: format!("daemon unreachable: {e}"), absent: false },
        TransportError::Undetermined { detail } => Attempt::Retry { reason: format!("daemon unreachable: {detail}"), absent: false },
        TransportError::LinkDown => Attempt::Retry { reason: "daemon unreachable: the host's link is down".into(), absent: false },
        other => Attempt::Retry { absent: other.is_endpoint_absent(), reason: other.to_string() },
    }
}

/// A failure after the dial.
fn row_error(e: RowError) -> Attempt {
    match e {
        RowError::RowMismatch { ours, theirs } => {
            Attempt::Terminal(format!("the row speaks row protocol {theirs} and this client {ours}: one side needs an update"))
        }
        RowError::Foreign => Attempt::Terminal("the peer behind the row is not this account's".into()),
        RowError::PathTooLong(path) => Attempt::Terminal(format!("the row socket path is too long: {}", path.display())),
        other => Attempt::Retry { reason: other.to_string(), absent: false },
    }
}

#[cfg(test)]
#[path = "attach_end_tests.rs"]
mod end_tests;
