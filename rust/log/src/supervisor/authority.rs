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
/// A `reset` that has not published its voyage by now is answered `failed`.
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
    voyage: Option<String>,
    /// Why the supervisor is terminal.
    detail: Option<String>,
    run: Option<LiveRun>,
    /// An admitted `end_run` waits for the run to end; its answer is left here by `run_ended` or `terminal`.
    ending: bool,
    end_answer: Option<EndRunRes>,
    /// An admitted `reset`: the new voyage the main thread publishes, and the answer it leaves.
    reset: Option<String>,
    resetting: bool,
    reset_answer: Option<ResetRes>,
    stop: Option<StopRequest>,
    terminal_since: Option<Instant>,
    /// The main thread is done; every wait ends.
    exiting: bool,
    /// The row socket's listener died and could not be bound again: nobody can reach the row.
    row_lost: bool,
}

pub(super) struct Shared {
    inner: Mutex<Inner>,
    changed: Condvar,
}

/// What the main thread does once the agent's run ended.
pub(super) enum After {
    Stop,
    Reset { new_voyage: String },
    /// A terminal supervisor with no stop asked lingers `TERMINAL_EXIT_GRACE` and exits.
    TerminalGrace,
}

fn failed_end(detail: &str) -> EndRunRes {
    EndRunRes { outcome: EndRunOutcome::Failed, detail: Some(bounded_detail(detail)) }
}

fn failed_reset(detail: &str) -> ResetRes {
    ResetRes { outcome: ResetOutcome::Failed, voyage: None, detail: Some(bounded_detail(detail)) }
}

impl Shared {
    pub(super) fn new() -> Shared {
        Shared {
            inner: Mutex::new(Inner {
                phase: SupervisorPhase::Starting,
                voyage: None,
                detail: None,
                run: None,
                ending: false,
                end_answer: None,
                reset: None,
                resetting: false,
                reset_answer: None,
                stop: None,
                terminal_since: None,
                exiting: false,
                row_lost: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn set_voyage(&self, voyage: &str) {
        self.lock().voyage = Some(voyage.to_string());
    }

    /// A run begins: its id, kill channel and signals are what `end_run`, `stop` and status read. `false` when a stop
    /// was asked before the agent started: nothing is started.
    pub(super) fn run_begins(&self, id: String, kill: mpsc::Sender<Command>, signals: Arc<RunSignals>) -> bool {
        let mut inner = self.lock();
        if inner.stop.is_some() {
            return false;
        }
        inner.phase = SupervisorPhase::Starting;
        inner.run = Some(LiveRun { id, kill, signals });
        true
    }

    /// The run ended with the agent gone: the supervisor rests until a reset or a stop.
    pub(super) fn run_ended(&self) {
        let mut inner = self.lock();
        inner.run = None;
        inner.phase = SupervisorPhase::EndedNoRespawn;
        if std::mem::take(&mut inner.ending) {
            inner.end_answer = Some(EndRunRes { outcome: EndRunOutcome::Ended, detail: None });
        }
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
        if std::mem::take(&mut inner.ending) {
            inner.end_answer = Some(failed_end(&detail));
        }
        inner.reset = None;
        if std::mem::take(&mut inner.resetting) {
            inner.reset_answer = Some(failed_reset(&detail));
        }
        inner.detail = Some(detail);
        self.changed.notify_all();
    }

    /// The reset's new voyage is published and its folder is there: the next run starts under it.
    pub(super) fn reset_done(&self, new_voyage: &str) {
        let mut inner = self.lock();
        inner.voyage = Some(new_voyage.to_string());
        inner.resetting = false;
        inner.reset_answer = Some(ResetRes { outcome: ResetOutcome::Reset, voyage: Some(new_voyage.to_string()), detail: None });
        self.changed.notify_all();
    }

    /// Blocks until the supervisor has something to do after the run: a stop, a reset, or the terminal grace over.
    pub(super) fn wait_after_run(&self) -> After {
        let mut inner = self.lock();
        loop {
            if inner.stop.is_some() {
                return After::Stop;
            }
            if let Some(new_voyage) = inner.reset.take() {
                return After::Reset { new_voyage };
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
        inner.ending || inner.stop.is_some()
    }

    /// An `end_run` is in flight (a stop is not one).
    pub(super) fn end_run_asked(&self) -> bool {
        self.lock().ending
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

    /// The phase the wire reports: `starting` becomes `ready` once the agent has started and has written, or has
    /// lived `READY_AFTER`.
    fn wire_phase(inner: &Inner) -> SupervisorPhase {
        if inner.phase != SupervisorPhase::Starting {
            return inner.phase;
        }
        match inner.run.as_ref().map(|r| &r.signals) {
            Some(s) if s.spawned_at().is_some_and(|at| s.output_seen() || at.elapsed() >= READY_AFTER) => {
                SupervisorPhase::Ready
            }
            _ => SupervisorPhase::Starting,
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
        let inner = self.lock();
        StatusRes {
            phase: Self::phase_name(Self::wire_phase(&inner)).to_string(),
            run: inner.run.as_ref().map(|r| r.id.clone()),
            voyage: inner.voyage.clone(),
        }
    }

    /// Admits an `end_run` for the live run `run` names, then waits for the run to end. The run fences it: another
    /// run's id changes nothing.
    fn end_run(&self, run: &str, _reason: &str) -> EndRunRes {
        let mut inner = self.lock();
        if inner.phase == SupervisorPhase::Terminal {
            return failed_end("the supervisor is in a terminal state");
        }
        let Some(live) = inner.run.as_ref() else {
            return failed_end("no run is currently live");
        };
        if live.id != run {
            return EndRunRes { outcome: EndRunOutcome::StaleRun, detail: None };
        }
        let kill = live.kill.clone();
        if inner.phase != SupervisorPhase::Starting {
            return failed_end("an end_run is currently in progress");
        }
        inner.phase = SupervisorPhase::Ending;
        inner.ending = true;
        inner.end_answer = None;
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
        let deadline = Instant::now() + END_RUN_BOUND;
        loop {
            if let Some(answer) = inner.end_answer.take() {
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

    /// Admits a `reset` from a resting supervisor, then waits for the main thread to publish the new voyage. The phase
    /// fences it: only `ended_no_respawn` admits one, and none while another is in progress.
    fn reset(&self) -> ResetRes {
        let mut inner = self.lock();
        if inner.phase != SupervisorPhase::EndedNoRespawn {
            return failed_reset(match inner.phase {
                _ if inner.resetting => "a reset is already in progress",
                SupervisorPhase::Ending => "an end_run is currently in progress",
                SupervisorPhase::Terminal => "the supervisor is in a terminal state",
                _ => "a run is currently live; end the run before resetting",
            });
        }
        inner.reset = Some(uuid::Uuid::now_v7().to_string());
        inner.resetting = true;
        inner.reset_answer = None;
        inner.phase = SupervisorPhase::Starting;
        self.changed.notify_all();
        let deadline = Instant::now() + RESET_BOUND;
        loop {
            if let Some(answer) = inner.reset_answer.take() {
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
