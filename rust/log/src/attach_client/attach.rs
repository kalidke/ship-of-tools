//! The attach worker: one thread that keeps a viewer attached to a row's socket.
//!
//! It waits until the row is viewed and the endpoint's link is up, dials once, attaches (a driver for a window, a
//! watcher for the wake), hands the snapshot and then the output to its sink, and carries the caller's input and
//! resizes to the row. A lost connection is redialed on platform's `Redial`; some failures end the episode for good.
//! The worker holds no screen: the sink's owner restores the snapshot into its parser.

use crate::lane::client::Endpoint;
use crate::lane::transport::TransportError;
use crate::row::{RowClient, RowError, RowEvent, RowWaker};
use sot_platform::host::redial::Redial;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The most input held while the row is not attached, in bytes; more is dropped and counted.
pub const HELD_INPUT_CAP: usize = 8192;
/// The most input queued for a row that is attached, in bytes: a paste fits, a stalled row does not grow without end.
const QUEUED_INPUT_CAP: usize = 4 * 1024 * 1024;
/// Output bytes the sink may hold before the worker stops reading the row, which then evicts a viewer that falls
/// behind and shows it a fresh snapshot on its redial.
const SINK_BOUND_BYTES: usize = 4 * 1024 * 1024;
/// The first wait and the cap of a redial: 250 ms doubling to 4 s, started over only after a connection that lasted
/// `STABLE`.
pub const REDIAL_FLOOR: Duration = Duration::from_millis(250);
pub const REDIAL_CAP: Duration = Duration::from_secs(4);
/// How long a row may stay unreachable, attempt after attempt, before the episode ends.
pub const UNREACHABLE_WINDOW: Duration = Duration::from_secs(120);
/// How long one dial, hello included, may take.
const DIAL_BOUND: Duration = Duration::from_secs(10);
/// How often an idle worker looks at its flags; a connected one wakes on the row's frames and on its caller.
const IDLE_TICK: Duration = Duration::from_millis(20);
/// How long a connected worker waits for a frame before it looks at its caller's commands.
const EVENT_TICK: Duration = Duration::from_millis(250);

/// What the worker tells its sink.
#[derive(Debug)]
pub enum WorkerEvent {
    /// The row's screen at attach, whole.
    Checkpoint(Vec<u8>),
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
    /// The pen was no longer this client's, and the client does not take it back.
    RefusedStale,
    /// The row may or may not have written the bytes; they are never sent again.
    DeliveryUnknown,
}

enum Command {
    Input(Vec<u8>),
    Resize(u16, u16),
}

/// State the caller and the worker share.
struct Shared {
    viewed: AtomicBool,
    shutdown: AtomicBool,
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
}

impl Shared {
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
        let shared = Arc::new(Shared {
            viewed: AtomicBool::new(true),
            shutdown: AtomicBool::new(false),
            attached: AtomicBool::new(false),
            queued_input: AtomicUsize::new(0),
            discarded: AtomicUsize::new(0),
            sink_bytes: AtomicUsize::new(0),
            waker: Mutex::new(None),
            recorded: AtomicU64::new(0),
            outcome: Mutex::new(None),
            run: Mutex::new(String::new()),
            size: Mutex::new((cols, rows)),
        });
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
        let shared = Arc::new(Shared {
            viewed: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            attached: AtomicBool::new(false),
            queued_input: AtomicUsize::new(0),
            discarded: AtomicUsize::new(0),
            sink_bytes: AtomicUsize::new(0),
            waker: Mutex::new(None),
            recorded: AtomicU64::new(0),
            outcome: Mutex::new(None),
            run: Mutex::new(String::new()),
            size: Mutex::new((80, 24)),
        });
        Self { shared, commands, handle: Mutex::new(None) }
    }

    /// A client off screen starts no dial; it finishes the attempt in flight and waits until it is viewed again.
    pub fn set_viewed(&self, viewed: bool) {
        self.shared.viewed.store(viewed, Ordering::Release);
    }

    /// Queues input for the row. Before the attach only [`HELD_INPUT_CAP`] bytes are held; the rest are dropped and
    /// counted. Returns whether the bytes were kept.
    pub fn send_input(&self, bytes: Vec<u8>) -> bool {
        let cap = if self.shared.attached.load(Ordering::Acquire) { QUEUED_INPUT_CAP } else { HELD_INPUT_CAP };
        let queued = self.shared.queued_input.load(Ordering::Acquire);
        if queued.saturating_add(bytes.len()) > cap {
            self.shared.discarded.fetch_add(1, Ordering::AcqRel);
            return false;
        }
        self.shared.queued_input.fetch_add(bytes.len(), Ordering::AcqRel);
        if self.commands.send(Command::Input(bytes)).is_err() {
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

    /// Ends the worker and waits up to `wait` for its thread; `true` when it ended.
    pub fn shutdown(&self, wait: Duration) -> bool {
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
    Lost,
}

/// How one attempt to connect and attach ended before it served.
enum Attempt {
    Retry(String),
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
        let mut unreachable_since: Option<Instant> = None;
        loop {
            if !self.wait_until_ready() {
                return;
            }
            let started = Instant::now();
            let lasted = match self.attempt() {
                Ok((mut client, driver)) => {
                    unreachable_since = None;
                    let ended = self.serve(&mut client, driver);
                    self.shared.attached.store(false, Ordering::Release);
                    *self.shared.waker.lock().unwrap_or_else(|p| p.into_inner()) = None;
                    if matches!(ended, Ended::Shutdown) {
                        return;
                    }
                    (self.sink)(WorkerEvent::Status("reconnecting\u{2026}".into()));
                    started.elapsed()
                }
                Err(Attempt::Terminal(reason)) => {
                    (self.sink)(WorkerEvent::Terminal(reason));
                    return;
                }
                Err(Attempt::Retry(reason)) => {
                    let since = *unreachable_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= UNREACHABLE_WINDOW {
                        (self.sink)(WorkerEvent::Terminal(format!("the row did not answer for {UNREACHABLE_WINDOW:?}: {reason}")));
                        return;
                    }
                    (self.sink)(WorkerEvent::Status(format!("reconnecting\u{2026} ({reason})")));
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
    fn attempt(&self) -> Result<(RowClient<E::Client>, bool), Attempt> {
        let deadline = Instant::now() + DIAL_BOUND;
        let conn = self.endpoint.connect_row_unchallenged(&self.row).map_err(dial_error)?;
        let mut client = RowClient::over(&self.endpoint, conn, "fe-attach", deadline).map_err(row_error)?;
        let (cols, rows) = *self.shared.size.lock().unwrap_or_else(|p| p.into_inner());
        let driver = !self.headless;
        let run = client.attach(cols, rows, driver).map_err(row_error)?;
        *self.shared.run.lock().unwrap_or_else(|p| p.into_inner()) = run;
        *self.shared.waker.lock().unwrap_or_else(|p| p.into_inner()) = Some(client.waker());
        // The snapshot comes before the first output, in chunks the last of which is flagged.
        let mut snapshot = Vec::new();
        let deadline = Instant::now() + DIAL_BOUND;
        loop {
            if self.stopping() {
                return Err(Attempt::Retry("ending".into()));
            }
            match client.next_event(EVENT_TICK).map_err(row_error)? {
                Some(RowEvent::Snapshot { bytes, last }) => {
                    snapshot.extend_from_slice(&bytes);
                    if last {
                        break;
                    }
                }
                Some(RowEvent::Demoted) | Some(RowEvent::Output(_)) => {}
                None => {
                    if Instant::now() >= deadline {
                        return Err(Attempt::Retry("the row sent no screen".into()));
                    }
                }
            }
        }
        (self.sink)(WorkerEvent::Checkpoint(snapshot));
        (self.sink)(WorkerEvent::Status("attached".into()));
        self.shared.attached.store(true, Ordering::Release);
        Ok((client, driver))
    }

    /// Follows the row until the connection ends or the worker is told to.
    fn serve(&self, client: &mut RowClient<E::Client>, mut driver: bool) -> Ended {
        let mut taken = false;
        loop {
            if self.stopping() {
                return Ended::Shutdown;
            }
            // The sink's owner has not consumed what it was given: stop reading, so the row's own queue fills and it
            // evicts a viewer that cannot keep up.
            if self.shared.sink_bytes.load(Ordering::Acquire) > SINK_BOUND_BYTES {
                std::thread::sleep(Duration::from_millis(2));
            } else {
                match client.next_event(EVENT_TICK) {
                    Ok(Some(RowEvent::Output(bytes))) => {
                        self.shared.sink_bytes.fetch_add(bytes.len(), Ordering::AcqRel);
                        (self.sink)(WorkerEvent::Output(bytes));
                    }
                    Ok(Some(RowEvent::Demoted)) => driver = false,
                    Ok(Some(RowEvent::Snapshot { .. })) | Ok(None) => {}
                    Err(_) => return Ended::Lost,
                }
            }
            while let Ok(command) = self.inbox.try_recv() {
                match command {
                    Command::Input(bytes) => {
                        self.shared.queued_input.fetch_sub(bytes.len(), Ordering::AcqRel);
                        if let Err(ended) = self.type_input(client, &bytes, &mut driver, &mut taken) {
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

    /// Types `bytes`, taking the pen first when this client does not hold it.
    fn type_input(&self, client: &mut RowClient<E::Client>, bytes: &[u8], driver: &mut bool, taken: &mut bool) -> Result<(), Ended> {
        if !*driver {
            // A watcher that typed once gives up the pen to whoever took it after, and does not fight for it.
            if self.headless && *taken {
                self.settle(InputOutcome::RefusedStale);
                return Ok(());
            }
            match client.take() {
                Ok(()) => {
                    *driver = true;
                    *taken = true;
                    if !self.headless {
                        let (cols, rows) = *self.shared.size.lock().unwrap_or_else(|p| p.into_inner());
                        let _ = client.resize(cols, rows);
                    }
                }
                Err(RowError::Refused(_)) => {
                    self.settle(InputOutcome::RefusedStale);
                    return Ok(());
                }
                Err(_) => {
                    self.settle(InputOutcome::DeliveryUnknown);
                    return Err(Ended::Lost);
                }
            }
        }
        match client.send_input(bytes) {
            Ok(written) => {
                self.shared.recorded.fetch_add(written as u64, Ordering::AcqRel);
                self.settle(InputOutcome::Recorded);
                Ok(())
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
            Err(_) => {
                self.settle(InputOutcome::DeliveryUnknown);
                Err(Ended::Lost)
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
        TransportError::Foreign => Attempt::Terminal("the row's socket belongs to another account".into()),
        TransportError::Refused { code, detail } => Attempt::Terminal(format!("the daemon refused the row ({code}): {detail}")),
        TransportError::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => {
            Attempt::Terminal(format!("access to the row was denied: {source}"))
        }
        other => Attempt::Retry(other.to_string()),
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
        other => Attempt::Retry(other.to_string()),
    }
}
