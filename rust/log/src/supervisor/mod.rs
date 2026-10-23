//! The supervisor: the one process of a row. `sot-capsule supervise` takes the row's fence (or adopts the claim it was
//! born holding), binds its row socket, restores the screen, starts the agent on a terminal from its main thread and serves
//! the row until the agent's end or a `stop`. The agent is the supervisor's own child: the supervisor holds the
//! terminal and the parser, so its end ends the agent (Linux: the kernel's parent-death signal for the forking
//! thread, which is this one; elsewhere the terminal's hangup or the job's close).
//!
//! # Phases
//!
//! `starting` (until the agent has written or lived two seconds), `ready`, `ending` (an `end_run` asked, the run
//! ending), `ended_no_respawn` (the agent is gone because an end was asked for; only a reset or a stop remains) and
//! `terminal` (sticky). The supervisor never starts the agent a second time on its own: an agent that ends unasked
//! ends the supervisor with a code the daemon's watchdog reads, and the watchdog's budget is the one restart budget.
//!
//! # Threads
//!
//! The main thread owns the run (`capsule::run`: the parser, the agent, the row socket's requests) and the reset. A
//! connection's thread of the row socket answers the daemon's management calls through `Shared` (`authority`): it asks
//! the run to end over the run's own channel and is told, by `Shared`, what became of it. A third thread watches the
//! row socket's listener and waits on an event, never on a clock.
//!
//! # Exit codes
//!
//! `0` after a stop; `69` when terminal; `70` when the fence is held by a live supervisor, which a launcher answers
//! by looking for that supervisor, never by starting a third; `75` when the agent could not be started and `76` when
//! it started and ended by itself, neither asked to stop (the launcher restarts, within its budget).

#![cfg(any(windows, target_os = "linux", target_os = "macos"))]

use crate::capsule::{CapsuleConfig, ExitKind, RunSignals};
use crate::SupervisorPhase;
use crate::row::{row_socket_path, RowRun, RowServer};
use std::collections::HashMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

mod authority;
#[cfg(unix)]
pub mod birth_claim;
pub mod journal;
pub(crate) use authority::self_pid_and_created;
use authority::{After, Shared};
use journal::pointer::{self, PointerState};

#[cfg(windows)]
type Agent = crate::capsule::producer::conpty::producer::ConptyProducer;
#[cfg(unix)]
type Agent = crate::capsule::producer::pty::PtyProducer;

/// The main thread's wait between looks at what a client asked.
const MAIN_LOOP_POLL: Duration = Duration::from_millis(100);
/// A terminal supervisor nobody stops lingers this long, serving status, and exits.
const TERMINAL_EXIT_GRACE: Duration = Duration::from_secs(2);
/// How long the exit waits for a `stop`'s reply to be delivered.
const STOP_REPLY_BOUND: Duration = Duration::from_secs(3);

/// The launcher's contract: `0` clean end, do not restart; `69` terminal, do not restart, surface it; `70` the
/// fence was already held by a live supervisor, so look for it and never start a third; `75` the agent could not be
/// started, restart with the same argv; `76` the agent started and ended by itself, restart without the arguments
/// meant for the first run only; anything else is a crash, restarted with the same argv.
pub const EXIT_CLEAN: i32 = 0;
pub const EXIT_TERMINAL: i32 = 69;
pub const EXIT_CONTENDED: i32 = 70;
pub const EXIT_AGENT_NOT_STARTED: i32 = 75;
pub const EXIT_AGENT_ENDED: i32 = 76;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Start,
    Resume,
}

pub struct SuperviseConfig {
    pub state_dir: PathBuf,
    pub mode: StartMode,
    pub producer_argv: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    /// Tokens (`--first-leg-only <token>`) kept in `producer_argv` until an agent has started in this process and
    /// stripped from every run after: the daemon passes a reauth's `--resume <id>` this way, so only the reauth's own
    /// run resumes by id. This module knows nothing about the agent or `--resume`.
    pub first_leg_only: Vec<String>,
    /// The fence claim this supervisor was born holding, and the channel it answers on once the claim is its own
    /// (`--claim-fd`, `--takeover-fd`). `None` for a supervisor nobody claimed for: it takes the fence itself.
    #[cfg(unix)]
    pub birth: Option<birth_claim::InheritedBirth>,
}

/// What `sot-capsule supervise` runs; every expected failure maps to [`EXIT_TERMINAL`], every success to
/// [`EXIT_CLEAN`].
pub fn supervise(config: SuperviseConfig) -> i32 {
    // Harden this process's inherited stdin and stdout, so nothing past the shared stderr reaches the agent.
    #[cfg(windows)]
    if let Err(e) = sot_platform::host::winhandle::harden_own_stdio(false) {
        note(format_args!("could not harden inherited stdin/stdout ({e}); continuing"));
    }
    match supervise_inner(config) {
        Ok(code) => code,
        Err(e) => {
            note(format_args!("{e}"));
            EXIT_TERMINAL
        }
    }
}

/// The fence this supervisor holds for its whole life.
enum Fence {
    Taken(#[allow(dead_code, reason = "held for its Drop")] journal::fence::SupervisorLock),
    #[cfg(unix)]
    Inherited(#[allow(dead_code, reason = "held for its descriptor")] birth_claim::BirthClaim),
}

/// Become the authority: adopt the claim this process was born holding and answer the parent that forked it, or, with
/// none, take the fence now. An adopted claim is the same locked open file description the parent took, so the fence
/// is never free between the claim and this process's first act.
fn take_authority(config: &SuperviseConfig) -> crate::Result<Fence> {
    #[cfg(unix)]
    if let Some(birth) = config.birth {
        let claim = birth_claim::BirthClaim::adopt(birth.claim_fd, &config.state_dir)?;
        #[cfg(feature = "native-barrier")]
        crate::test_barrier::hold("claim_adopted");
        let (pid, created) = authority::self_pid_and_created().unwrap_or((0, 0));
        // The acknowledgement is the parent's bookkeeping: a parent that is gone has nothing to be told, and one that
        // missed it keeps its own copy of the claim until this process ends. The claim is this supervisor's either way.
        if let Err(e) = birth_claim::acknowledge_takeover(birth.takeover_fd, birth_claim::Takeover { pid, created }) {
            note(format_args!("takeover not acknowledged to the parent ({e}); the claim is this supervisor's"));
        }
        return Ok(Fence::Inherited(claim));
    }
    journal::fence::lock_supervisor(&config.state_dir).map(Fence::Taken)
}

fn supervise_inner(config: SuperviseConfig) -> crate::Result<i32> {
    init_process_globals(&config)?;
    std::fs::create_dir_all(voyages_dir(&config.state_dir))?;

    #[cfg(all(unix, feature = "native-barrier"))]
    crate::test_barrier::hold("pre_fence");

    let _fence = match take_authority(&config) {
        Ok(f) => f,
        // `Error::State` is the one error `lock_supervisor` returns for "already held", which is not a failure of
        // this supervisor: another one lives. A bootstrap or I/O failure stays terminal.
        Err(e @ crate::Error::State(_)) => {
            note(format_args!("the fence is already held by a live supervisor: {e}"));
            return Ok(EXIT_CONTENDED);
        }
        Err(e) => {
            note(format_args!("could not become the authority: {e}"));
            return Ok(EXIT_TERMINAL);
        }
    };

    let h = sot_platform::host::state_dir::state_dir_hash(&config.state_dir);
    let shared = Arc::new(Shared::new());
    let row = RowRun::new("", "starting");
    {
        let shared = Arc::clone(&shared);
        row.set_phase_source(Box::new(move || shared.phase_text().to_string()));
    }
    row.set_management(Box::new(SharedHandle(Arc::clone(&shared))));
    // The row socket is the way in: a row nobody can reach is not worth an agent, so a bind failure ends this supervisor
    // before one starts, as a row that did not start: exit 75, which rows' watchdog counts against its one restart budget
    // (a failure that lasts ends the row terminal there, one that passes costs a restart).
    let row_path = match row_socket_path(&h) {
        Ok(path) => path,
        Err(e) => {
            note(format_args!("could not bind the row socket: {e}"));
            return Ok(EXIT_AGENT_NOT_STARTED);
        }
    };
    let (watch_tx, watch_rx) = mpsc::channel();
    let row_server = match RowServer::bind_watched(&row_path, Arc::clone(&row), death_event(&watch_tx)) {
        Ok(server) => server,
        Err(e) => {
            note(format_args!("could not bind the row socket: {e}"));
            return Ok(EXIT_AGENT_NOT_STARTED);
        }
    };
    // A listener that dies is bound once more; one that dies again, or cannot be bound, ends the run unrequested.
    let row_watch = {
        let (shared, row, tx) = (Arc::clone(&shared), Arc::clone(&row), watch_tx.clone());
        let path = row_path;
        std::thread::Builder::new()
            .name("row-watch".into())
            .spawn(move || watch_row_listener(row_server, &path, &row, &shared, &watch_rx, &tx))
            .map_err(crate::Error::Io)?
    };
    let code = drive(&config, &shared, &row);
    shared.finish();
    let _ = watch_tx.send(Watch::Stop);
    if row_watch.join().is_err() {
        note(format_args!("the row listener's watch panicked"));
    }
    Ok(code)
}

/// What the row listener's watch waits on: the acceptor's death, or the end of the supervisor.
enum Watch {
    Died,
    Stop,
}

fn death_event(tx: &mpsc::Sender<Watch>) -> impl Fn() + Send + Sync + 'static {
    let tx = Mutex::new(tx.clone());
    move || {
        let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(Watch::Died);
    }
}

/// The row socket's management, on the one `Shared`: the row owns a `Box`, the supervisor keeps its `Arc`.
struct SharedHandle(Arc<Shared>);

impl crate::row::Management for SharedHandle {
    fn status(&self) -> crate::row::StatusRes {
        self.0.status()
    }
    fn end_run(&self, run: &str, reason: &str) -> crate::row::EndRunRes {
        self.0.end_run(run, reason)
    }
    fn reset(&self) -> crate::row::ResetRes {
        self.0.reset()
    }
    fn stop(&self, conn: crate::row::ConnId) -> crate::row::StopRes {
        self.0.stop(conn)
    }
    fn conn_gone(&self, conn: crate::row::ConnId) {
        self.0.conn_gone(conn);
    }
}

/// Holds the row server for the supervisor's life and watches its acceptor, blocked until it dies or the supervisor is
/// done. A dead acceptor is replaced once; a second death or a failed bind is `Shared::row_lost`, which ends the run
/// unrequested so rows' watchdog decides through its one restart budget. Returns, dropping the server, on `Stop`.
fn watch_row_listener(
    mut server: RowServer,
    path: &Path,
    row: &Arc<RowRun>,
    shared: &Shared,
    events: &mpsc::Receiver<Watch>,
    tx: &mpsc::Sender<Watch>,
) {
    let mut rebound = false;
    while let Ok(Watch::Died) = events.recv() {
        // A death that races the end of the supervisor is not worth a rebind.
        if shared.is_exiting() {
            return;
        }
        note(format_args!("the row socket's listener died"));
        drop(server);
        if rebound {
            shared.row_lost();
            return;
        }
        rebound = true;
        match RowServer::bind_watched(path, Arc::clone(row), death_event(tx)) {
            Ok(s) => server = s,
            Err(e) => {
                note(format_args!("could not bind the row socket again: {e}"));
                shared.row_lost();
                return;
            }
        }
    }
}

/// The main thread's life: discover the voyage, run the agent, and after each run wait for what a client asks.
fn drive(config: &SuperviseConfig, shared: &Shared, row: &Arc<RowRun>) -> i32 {
    // Test seam: holds the supervisor in `starting` for the named milliseconds, before it has a voyage, so a test can
    // prove a caller waits for a row to rest rather than deciding from a snapshot mid-start. Inert unless named.
    if let Ok(ms) = std::env::var("SOT_TEST_RECOVERY_DELAY_MS").unwrap_or_default().parse::<u64>() {
        std::thread::sleep(Duration::from_millis(ms));
    }
    let mut voyage = match discover_or_mint_voyage(&config.state_dir) {
        Ok(v) => v,
        Err(e) => {
            shared.terminal(bounded_detail(format!("{e}")));
            return leave(shared);
        }
    };
    shared.set_voyage(&voyage);
    let mut agent_ran = false;
    // A `--resume` over a run that was asked to end does not start another: it rests until a reset or a stop.
    let mut rest_first = config.mode == StartMode::Resume
        && crate::capsule::read_ended(&voyage_root_path(&config.state_dir, &voyage)).ok().flatten().is_some_and(|e| e.requested);
    loop {
        let ended = if std::mem::take(&mut rest_first) {
            RunEnd::Ended { started: false }
        } else {
            run_agent(config, shared, row, &voyage, agent_ran)
        };
        match ended {
            RunEnd::NotStarted => return leave(shared),
            RunEnd::Ended { started } => {
                agent_ran |= started;
                shared.run_ended();
            }
            // Nobody asked for this end: the supervisor ends with the code the daemon's watchdog restarts on, unless a
            // stop is already on its way.
            RunEnd::Unasked(_) if shared.stop_asked() => return finish(shared),
            RunEnd::Unasked(code) => return code,
            RunEnd::Failed(detail) => shared.terminal(detail),
        }
        match shared.wait_after_run() {
            After::Stop => return finish(shared),
            After::TerminalGrace => return EXIT_TERMINAL,
            After::RowLost => return EXIT_AGENT_NOT_STARTED,
            After::Reset { new_voyage } => match pointer::reset(&config.state_dir, &new_voyage) {
                Ok(()) => {
                    shared.reset_done(&new_voyage);
                    voyage = new_voyage;
                }
                Err(e) => shared.terminal(bounded_detail(format!("reset: {e}"))),
            },
        }
    }
}

/// No run is left and none can start: a stop ends the supervisor, and without one the terminal grace does.
fn leave(shared: &Shared) -> i32 {
    match shared.wait_after_run() {
        After::Stop => finish(shared),
        After::RowLost => EXIT_AGENT_NOT_STARTED,
        _ => EXIT_TERMINAL,
    }
}

/// A stop ended the run (or none was live): the reply is delivered, then the exit code says whether the supervisor
/// was terminal when it was stopped.
fn finish(shared: &Shared) -> i32 {
    shared.wait_stop_delivered(STOP_REPLY_BOUND);
    if shared.is_terminal() {
        EXIT_TERMINAL
    } else {
        EXIT_CLEAN
    }
}

enum RunEnd {
    /// A stop was asked before the agent started.
    NotStarted,
    /// The run is over because an end was asked for (or a resume rests over one); `started` says the agent was started
    /// at all.
    Ended { started: bool },
    /// The agent could not be started or ended by itself, with no end asked for: the exit code to leave with.
    Unasked(i32),
    Failed(String),
}

/// One run of the agent, to its end, on this thread: the kernel's parent-death signal on Linux follows the thread
/// that forks the agent, and this one lives as long as the process.
fn run_agent(config: &SuperviseConfig, shared: &Shared, row: &Arc<RowRun>, voyage: &str, agent_ran: bool) -> RunEnd {
    if shared.is_row_lost() {
        return RunEnd::Unasked(EXIT_AGENT_NOT_STARTED);
    }
    let (kill, commands) = mpsc::channel();
    let signals = Arc::new(RunSignals::default());
    // Each agent start is a run of its own on the row socket, ordered by its UUIDv7.
    let run_id = uuid::Uuid::now_v7().to_string();
    row.set_run(run_id.clone());
    signals.attach_row(Arc::clone(row));
    if !shared.run_begins(run_id, kill, Arc::clone(&signals)) {
        return RunEnd::NotStarted;
    }
    let capsule = CapsuleConfig {
        voyage_root: voyage_root_path(&config.state_dir, voyage),
        screen_path: Some(config.state_dir.join("screen")),
        argv: agent_argv(config, agent_ran),
        cols: config.cols,
        rows: config.rows,
        signals: Some(Arc::clone(&signals)),
    };
    let result = crate::capsule::run::<Agent>(capsule, commands);
    // An acknowledged end_run is a requested end even if the agent ended first: the voyage records it before the answer.
    // An end that cannot be recorded is not an end: a later `--resume` would read no request and start another agent,
    // so the supervisor is terminal and the caller is told it failed.
    if shared.end_run_asked() {
        if let Err(e) = mark_end_requested(&voyage_root_path(&config.state_dir, voyage)) {
            return RunEnd::Failed(bounded_detail(format!("the requested end could not be recorded: {e}")));
        }
    }
    // The row socket was lost, so the run was told to stop: a row that did not start, whatever the agent did.
    if shared.is_row_lost() {
        return RunEnd::Unasked(EXIT_AGENT_NOT_STARTED);
    }
    match result {
        Ok(summary) => match summary.exit_kind {
            ExitKind::SpawnFailed if shared.end_asked() => RunEnd::Ended { started: false },
            ExitKind::SpawnFailed => {
                note(format_args!("the agent could not be started"));
                RunEnd::Unasked(EXIT_AGENT_NOT_STARTED)
            }
            ExitKind::ProducerExited | ExitKind::Requested => {
                note(format_args!("the agent ended: {:?} ({:?})", summary.exit_code, summary.exit_kind));
                if summary.exit_kind == ExitKind::ProducerExited && !shared.end_asked() {
                    RunEnd::Unasked(EXIT_AGENT_ENDED)
                } else {
                    RunEnd::Ended { started: signals.spawned_at().is_some() }
                }
            }
        },
        Err(e) => RunEnd::Failed(bounded_detail(format!("the run failed: {e}"))),
    }
}

/// The agent's argv for the next run: `first_leg_only` tokens stay while no agent has started in this process (so a
/// first run that failed before running anything is retried with them) and go from every run after.
fn agent_argv(config: &SuperviseConfig, agent_ran: bool) -> Vec<String> {
    if agent_ran {
        config.producer_argv.iter().filter(|a| !config.first_leg_only.contains(a)).cloned().collect()
    } else {
        config.producer_argv.clone()
    }
}

fn init_process_globals(config: &SuperviseConfig) -> crate::Result<()> {
    // Set before anything else: every diagnostic below goes through `note`, which reads this.
    let _ = NOTE_PREFIX.set(format!(
        "sot-capsule supervise[{}]",
        config.state_dir.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
    ));
    // SIGCHLD is set to its default, never merely assumed: whatever launched this process may have left it ignored
    // across `exec`, which would reap the agent before this process could read how it ended.
    #[cfg(unix)]
    {
        // SAFETY: setting a signal disposition to the default is a plain syscall.
        if unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(crate::Error::Io(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------

fn voyages_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("voyages")
}

/// The leg's folder of `voyage_id`, `<state_dir>/voyages/<id>/`: one place derives it, for the daemon's destroy proof
/// as for this module.
pub fn voyage_root_path(state_dir: &Path, voyage_id: &str) -> PathBuf {
    voyages_dir(state_dir).join(voyage_id)
}

/// The voyage the pointer names, minted when there is none: a `--resume` over a folder with no pointer starts the agent
/// like a `--start`, since the supervisor that left it never got as far as publishing one.
fn discover_or_mint_voyage(state_dir: &Path) -> crate::Result<String> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) => Ok(id),
        PointerState::NotFound => {
            std::fs::create_dir_all(voyages_dir(state_dir))?;
            let id = uuid::Uuid::now_v7().to_string();
            crate::capsule::bootstrap_voyage_folder(&voyage_root_path(state_dir, &id))?;
            pointer::publish(state_dir, &id)?;
            Ok(id)
        }
        PointerState::Corrupt => Err(crate::Error::State("drawer.voyage is corrupt".into())),
        PointerState::OtherIo(e) => Err(e.into()),
    }
}

/// Records on the voyage that the run was asked to end. Test seam: `SOT_TEST_END_MARK_FAILS` makes the write fail,
/// inert unless named.
fn mark_end_requested(voyage_root: &Path) -> crate::Result<()> {
    if std::env::var_os("SOT_TEST_END_MARK_FAILS").is_some() {
        return Err(crate::Error::State("test: the requested end cannot be written".into()));
    }
    crate::capsule::mark_end_requested(voyage_root, "end_run")
}

/// The longest detail a supervisor keeps of a failure, in bytes.
const MAX_DETAIL_LEN: usize = 128;

/// Truncate `detail` to fit within [`MAX_DETAIL_LEN`] bytes, on a UTF-8 boundary.
fn bounded_detail(detail: impl Into<String>) -> String {
    let mut s = detail.into();
    if s.len() > MAX_DETAIL_LEN {
        let mut cut = MAX_DETAIL_LEN;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// One process-global prefix, set once, naming the row (the state folder's name), so lines from many supervisors
/// sharing the daemon's one log file are distinguishable.
static NOTE_PREFIX: OnceLock<String> = OnceLock::new();

/// One complete diagnostic line, as one `write_all` on stderr: `O_APPEND` keeps a single `write(2)` intact between
/// processes appending to the same file, and nothing else does.
fn note(args: fmt::Arguments<'_>) {
    let prefix = NOTE_PREFIX.get().map(String::as_str).unwrap_or("sot-capsule supervise");
    let line = format!("{prefix}: {args}\n");
    let _ = std::io::stderr().write_all(line.as_bytes());
}
