//! What the daemon may ask of the one process of a row, and the state the main thread and the row socket's connection
//! threads share. A connection's thread answers a management call through `Shared`; the main thread owns the agent and
//! tells `Shared` what became of it (its `run_*` methods).

use super::*;
use crate::capsule::{Command, RunSignals};
use crate::row::{
    ConnId, EndRunOutcome, EndRunRes, Management, ResetOutcome, ResetRes, StatusRes, StopOutcome, StopRes,
};

/// A `starting` supervisor reads `ready` this long after its agent started, if the agent has not written before.
const READY_AFTER: Duration = Duration::from_secs(2);
/// An `end_run` that has not seen the run end by now is answered `ending`, so the caller's own 30 s bound never races it.
const END_RUN_BOUND: Duration = Duration::from_secs(25);
/// A `reset` that has not published by now is answered `failed`.
const RESET_BOUND: Duration = Duration::from_secs(25);

/// The agent's run in progress: the channel that ends it, and what it tells its owner.
struct LiveRun {
    id: String,
    kill: mpsc::Sender<Command>,
    signals: Arc<RunSignals>,
}

struct StopRequest {
    /// The first connection whose `stop` was admitted; the exit waits for its reply to be delivered or given up on.
    primary_conn: ConnId,
    delivered: bool,
}

struct Inner {
    /// `starting`, `ending`, `ended_no_respawn` or `terminal`; `ready` is derived (`wire_phase`).
    phase: SupervisorPhase,
    run: Option<LiveRun>,
    /// The run's agent is gone and the main thread has decided whether an end was asked for: no end is admitted.
    run_over: bool,
    /// The last run that began: a resting supervisor reports it, so the daemon can tell the run a rest follows from a newer one.
    last_run: Option<String>,
    /// An admitted `end_run` waits for its run to end; the answer `run_ended` or `terminal` leaves is kept under the
    /// admission's number, so another run's admission never clears it. `ending` is the admission waiting now.
    ending: Option<u64>,
    end_answers: HashMap<u64, EndRunRes>,
    /// An admitted `reset` the main thread has not yet published (by removing the end record), and the answers it
    /// leaves, by admission number.
    reset: bool,
    resetting: Option<u64>,
    reset_answers: HashMap<u64, ResetRes>,
    /// The last admission number given to an `end_run` or a `reset`.
    admissions: u64,
    stop: Option<StopRequest>,
    terminal_since: Option<Instant>,
    /// The main thread is done; every wait ends.
    exiting: bool,
    /// The row socket's listener died and could not be bound again: nobody can reach the row.
    row_lost: bool,
    /// Where each change of the status is sent, and the last one sent ([`Shared::note`]).
    notes: Option<mpsc::Sender<StatusRes>>,
    noted: Option<StatusRes>,
}

pub(super) struct Shared {
    inner: Mutex<Inner>,
    changed: Condvar,
}

/// What the main thread does once the agent's run ended.
pub(super) enum After {
    Stop,
    Reset,
    /// The row socket's listener is lost for good: nothing can reach this supervisor, so it ends with 75.
    RowLost,
    /// A terminal supervisor with no stop asked lingers `TERMINAL_EXIT_GRACE` and exits.
    TerminalGrace,
}

fn failed_end(detail: &str) -> EndRunRes {
    EndRunRes { outcome: EndRunOutcome::Failed, detail: Some(bounded_detail(detail)) }
}

fn failed_reset(detail: &str) -> ResetRes {
    ResetRes { outcome: ResetOutcome::Failed, detail: Some(bounded_detail(detail)) }
}

impl Shared {
    pub(super) fn new() -> Shared {
        Shared {
            inner: Mutex::new(Inner {
                phase: SupervisorPhase::Starting,
                run: None,
                run_over: false,
                last_run: None,
                ending: None,
                end_answers: HashMap::new(),
                reset: false,
                resetting: None,
                reset_answers: HashMap::new(),
                admissions: 0,
                stop: None,
                terminal_since: None,
                exiting: false,
                row_lost: false,
                notes: None,
                noted: None,
            }),
            changed: Condvar::new(),
        }
    }

    /// Starts the ordered record of what the status reports: from here every change of phase or run is sent down `notes`,
    /// with its value, under the lock that made it. Returns the status as it stands, which the caller announces first.
    pub(super) fn set_announcer(&self, notes: mpsc::Sender<StatusRes>) -> StatusRes {
        let mut inner = self.lock();
        let now = Self::status_of(&inner);
        inner.noted = Some(now.clone());
        inner.notes = Some(notes);
        now
    }

    /// Ends the record: what was sent stays in the channel for the announcer to deliver, and nothing more is sent.
    pub(super) fn close_announcer(&self) {
        self.lock().notes = None;
    }

    /// Sends the status down the record if it differs from the last one sent. A send never blocks, so this runs under the
    /// lock: the order of the record is the order of the mutations, and no announcement ever rereads state that moved on.
    fn note(inner: &mut Inner) {
        let now = Self::status_of(inner);
        if inner.noted.as_ref() != Some(&now) {
            if let Some(notes) = &inner.notes {
                let _ = notes.send(now.clone());
            }
            inner.noted = Some(now);
        }
    }

    fn status_of(inner: &Inner) -> StatusRes {
        StatusRes { phase: Self::phase_name(Self::wire_phase(inner)).to_string(), run: inner.last_run.clone() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A run begins: its id, kill channel and signals are what `end_run`, `stop` and status read. `false` when a stop
    /// was asked before the agent started: nothing is started.
    pub(super) fn run_begins(&self, id: String, kill: mpsc::Sender<Command>, signals: Arc<RunSignals>) -> bool {
        let mut inner = self.lock();
        // A lost listener admits no run: an agent nobody can reach is not worth starting.
        if inner.stop.is_some() || inner.row_lost || inner.phase == SupervisorPhase::Terminal {
            return false;
        }
        inner.phase = SupervisorPhase::Starting;
        inner.run_over = false;
        inner.last_run = Some(id.clone());
        inner.run = Some(LiveRun { id, kill, signals });
        Self::note(&mut inner);
        true
    }

    /// The run ended with the agent gone: the supervisor rests until a reset or a stop.
    pub(super) fn run_ended(&self) {
        let mut inner = self.lock();
        inner.run = None;
        inner.phase = SupervisorPhase::EndedNoRespawn;
        if let Some(admission) = inner.ending.take() {
            inner.end_answers.insert(admission, EndRunRes { outcome: EndRunOutcome::Ended, detail: None });
        }
        Self::note(&mut inner);
        self.changed.notify_all();
    }

    /// The supervisor can go no further: the agent could not run, or an internal step failed. Sticky.
    pub(super) fn terminal(&self, detail: String) {
        let mut inner = self.lock();
        if inner.phase == SupervisorPhase::Terminal {
            return;
        }
        note(format_args!("terminal: {detail}"));
        // A dropped sender is not a kill: the agent is told to end, with no request recorded, before the run is let go.
        if let Some(run) = inner.run.take() {
            let _ = run.kill.send(Command::Stop);
        }
        inner.phase = SupervisorPhase::Terminal;
        inner.terminal_since = Some(Instant::now());
        if let Some(admission) = inner.ending.take() {
            inner.end_answers.insert(admission, failed_end(&detail));
        }
        inner.reset = false;
        if let Some(admission) = inner.resetting.take() {
            inner.reset_answers.insert(admission, failed_reset(&detail));
        }
        Self::note(&mut inner);
        self.changed.notify_all();
    }

    /// The reset is published, the end record replaced: the next run starts clean.
    pub(super) fn reset_done(&self) {
        let mut inner = self.lock();
        if let Some(admission) = inner.resetting.take() {
            inner.reset_answers.insert(admission, ResetRes { outcome: ResetOutcome::Reset, detail: None });
        }
        self.changed.notify_all();
    }

    /// Blocks until the supervisor has something to do after the run: a stop, a reset, or the terminal grace over.
    pub(super) fn wait_after_run(&self) -> After {
        let mut inner = self.lock();
        loop {
            if inner.stop.is_some() {
                return After::Stop;
            }
            if inner.row_lost {
                return After::RowLost;
            }
            if std::mem::take(&mut inner.reset) {
                return After::Reset;
            }
            if let Some(since) = inner.terminal_since {
                if since.elapsed() >= TERMINAL_EXIT_GRACE {
                    return After::TerminalGrace;
                }
            }
            inner = self.changed.wait_timeout(inner, MAIN_LOOP_POLL).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// An `end_run` is in flight or a stop was asked: the run's end is not the agent's own.
    pub(super) fn end_asked(&self) -> bool {
        let inner = self.lock();
        inner.ending.is_some() || inner.stop.is_some()
    }

    /// The run's agent is gone: no end is admitted from here on, and whether one was admitted is the answer, read under
    /// the lock that closes admission. Every end answered `ended` was admitted before this, so the caller records it.
    pub(super) fn close_run(&self) -> bool {
        let mut inner = self.lock();
        inner.run_over = true;
        inner.ending.is_some()
    }

    /// The row socket is gone for good: the live run, if any, is told to end with no request recorded, and no run
    /// starts after it.
    pub(super) fn row_lost(&self) {
        let mut inner = self.lock();
        inner.row_lost = true;
        if let Some(run) = &inner.run {
            let _ = run.kill.send(Command::Stop);
        }
        self.changed.notify_all();
    }

    /// The supervisor is done: a listener that dies now is no loss to rebind.
    pub(super) fn is_exiting(&self) -> bool {
        self.lock().exiting
    }

    pub(super) fn is_row_lost(&self) -> bool {
        self.lock().row_lost
    }

    pub(super) fn stop_asked(&self) -> bool {
        self.lock().stop.is_some()
    }

    pub(super) fn is_terminal(&self) -> bool {
        self.lock().phase == SupervisorPhase::Terminal
    }

    /// A stop was asked: the main thread leaves once the reply is delivered or given up on.
    pub(super) fn wait_stop_delivered(&self, bound: Duration) {
        let deadline = Instant::now() + bound;
        let mut inner = self.lock();
        while inner.stop.as_ref().is_some_and(|s| !s.delivered) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            inner = self.changed.wait_timeout(inner, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// The main thread is done: every wait a connection's thread is in ends.
    pub(super) fn finish(&self) {
        self.lock().exiting = true;
        self.changed.notify_all();
    }

    /// The phase the wire reports: `starting` becomes `ready` once the agent has lived `READY_AFTER`: an agent that
    /// writes its error and exits within that time was never ready.
    fn wire_phase(inner: &Inner) -> SupervisorPhase {
        if inner.phase != SupervisorPhase::Starting {
            return inner.phase;
        }
        match inner.run.as_ref().map(|r| &r.signals) {
            Some(s) if s.spawned_at().is_some_and(|at| at.elapsed() >= READY_AFTER) => {
                SupervisorPhase::Ready
            }
            _ => SupervisorPhase::Starting,
        }
    }

    /// Admits an `end_run`: the admission number its answer is kept under, or the answer to give at once.
    pub(super) fn admit_end(&self, run: &str) -> Result<u64, EndRunRes> {
        let mut inner = self.lock();
        if inner.phase == SupervisorPhase::Terminal {
            return Err(failed_end("the supervisor is in a terminal state"));
        }
        let Some(live) = inner.run.as_ref().filter(|_| !inner.run_over) else {
            return Err(failed_end("no run is currently live"));
        };
        if live.id != run {
            return Err(EndRunRes { outcome: EndRunOutcome::StaleRun, detail: None });
        }
        let kill = live.kill.clone();
        if inner.phase != SupervisorPhase::Starting {
            return Err(failed_end("an end_run is currently in progress"));
        }
        inner.admissions += 1;
        let admission = inner.admissions;
        inner.phase = SupervisorPhase::Ending;
        inner.ending = Some(admission);
        // Test seam: the run learns of the request this many milliseconds after it was admitted, so a test can end the
        // agent in between. Inert unless named.
        match std::env::var("SOT_TEST_END_RUN_HOLD_MS").ok().and_then(|s| s.parse::<u64>().ok()) {
            Some(ms) => {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(ms));
                    let _ = kill.send(Command::Kill);
                });
            }
            None => {
                let _ = kill.send(Command::Kill);
            }
        }
        Self::note(&mut inner);
        Ok(admission)
    }

    /// Waits for the answer kept under `admission`: the run's own end, never another run's.
    pub(super) fn wait_end(&self, admission: u64) -> EndRunRes {
        let deadline = Instant::now() + END_RUN_BOUND;
        let mut inner = self.lock();
        loop {
            if let Some(answer) = inner.end_answers.remove(&admission) {
                return answer;
            }
            if inner.exiting {
                return failed_end("the supervisor is ending");
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return EndRunRes { outcome: EndRunOutcome::Ending, detail: None };
            }
            inner = self.changed.wait_timeout(inner, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Admits a `reset`: the admission number its answer is kept under, or the answer to give at once.
    pub(super) fn admit_reset(&self) -> Result<u64, ResetRes> {
        let mut inner = self.lock();
        if inner.phase != SupervisorPhase::EndedNoRespawn {
            return Err(failed_reset(match inner.phase {
                _ if inner.resetting.is_some() => "a reset is already in progress",
                SupervisorPhase::Ending => "an end_run is currently in progress",
                SupervisorPhase::Terminal => "the supervisor is in a terminal state",
                _ => "a run is currently live; end the run before resetting",
            }));
        }
        inner.admissions += 1;
        let admission = inner.admissions;
        inner.reset = true;
        inner.resetting = Some(admission);
        inner.phase = SupervisorPhase::Starting;
        Self::note(&mut inner);
        self.changed.notify_all();
        Ok(admission)
    }

    /// Waits for the answer kept under `admission`.
    pub(super) fn wait_reset(&self, admission: u64) -> ResetRes {
        let deadline = Instant::now() + RESET_BOUND;
        let mut inner = self.lock();
        loop {
            if let Some(answer) = inner.reset_answers.remove(&admission) {
                return answer;
            }
            if inner.exiting {
                return failed_reset("the supervisor is ending");
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return failed_reset("the reset did not complete in time");
            }
            inner = self.changed.wait_timeout(inner, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn phase_name(phase: SupervisorPhase) -> &'static str {
        match phase {
            SupervisorPhase::Starting => "starting",
            SupervisorPhase::Ready => "ready",
            SupervisorPhase::Ending => "ending",
            SupervisorPhase::EndedNoRespawn => "ended_no_respawn",
            SupervisorPhase::Terminal => "terminal",
        }
    }

    /// The phase the wire reports, as the row socket's hello names it.
    pub(super) fn phase_text(&self) -> &'static str {
        Self::phase_name(Self::wire_phase(&self.lock()))
    }
}

impl Management for Shared {
    fn status(&self) -> StatusRes {
        Self::status_of(&self.lock())
    }

    /// `ready` is a clock: the loop asks, each tick, whether the clock has changed the status.
    fn poll(&self) {
        Self::note(&mut self.lock());
    }

    /// Admits an `end_run` for the live run `run` names, then waits for the run to end. The run fences it: another
    /// run's id changes nothing.
    fn end_run(&self, run: &str, _reason: &str) -> EndRunRes {
        match self.admit_end(run) {
            Ok(admission) => self.wait_end(admission),
            Err(answer) => answer,
        }
    }

    /// Admits a `reset` from a resting supervisor, then waits for the main thread to publish it. The phase
    /// fences it: only `ended_no_respawn` admits one, and none while another is in progress.
    fn reset(&self) -> ResetRes {
        match self.admit_reset() {
            Ok(admission) => self.wait_reset(admission),
            Err(answer) => answer,
        }
    }

    fn stop(&self, conn: ConnId) -> StopRes {
        let mut inner = self.lock();
        if inner.stop.is_none() {
            inner.stop = Some(StopRequest { primary_conn: conn, delivered: false });
            if let Some(run) = &inner.run {
                let _ = run.kill.send(Command::Stop);
            }
        }
        self.changed.notify_all();
        StopRes { outcome: StopOutcome::Stopping }
    }

    /// A connection ended; a stop's primary connection ending is its reply delivered or given up on.
    fn conn_gone(&self, id: ConnId) {
        let mut inner = self.lock();
        if let Some(stop) = inner.stop.as_mut() {
            if stop.primary_conn == id && !stop.delivered {
                stop.delivered = true;
                self.changed.notify_all();
            }
        }
    }
}

#[cfg(windows)]
pub(crate) fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle needing no close; the four FILETIME out-params are plain,
    // stack-local structs, valid to write into regardless of the call's outcome.
    unsafe {
        let pid = GetCurrentProcessId();
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        if GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let created = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        Ok((pid, created))
    }
}

/// This process's `(pid, start identity)` pair: on Linux the start-time ticks the socket challenge reports for a peer.
#[cfg(target_os = "linux")]
pub(crate) fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    Ok((std::process::id(), sot_platform::identity::challenge_unix::self_start_ticks()?))
}

/// On macOS the identity is the kernel's `pidversion`, the value the challenge reads out of the peer's audit token;
/// a start time here would make every challenge `Foreign`.
#[cfg(target_os = "macos")]
pub(crate) fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    Ok((std::process::id(), u64::from(sot_platform::identity::challenge_macos::self_pidversion()?)))
}

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;
