//! The supervisor: the one process of a row. `sot-capsule supervise` takes the row's fence (or adopts the claim it was
//! born holding), binds its lane, restores the screen, starts the agent on a terminal from its main thread and serves
//! the row until the agent's end or a `stop`. The agent is the supervisor's own child: the supervisor holds the
//! terminal and the parser, so its end ends the agent (Linux: the kernel's parent-death signal for the forking
//! thread, which is this one; elsewhere the terminal's hangup or the job's close).
//!
//! # Phases
//!
//! `starting` (until the agent has written or lived two seconds), `ready`, `ending` (an `end_run` asked, the run
//! ending), `ended_no_respawn` (the agent is gone; only a reset or a stop remains) and `terminal` (sticky). The
//! supervisor never starts the agent a second time on its own: the daemon's watchdog is the one restart budget.
//!
//! # Threads
//!
//! The main thread owns the run (`capsule::run`: the parser, the agent, the attach lane) and the reset. A second
//! thread serves the supervisor lane (`authority`) and reaches the run through `Shared`: it asks the run to end over
//! the run's own channel and is told, by `Shared`, what became of it.
//!
//! # Exit codes
//!
//! `0` after a stop; `69` when terminal; `70` when the fence is held by a live supervisor, which a launcher answers
//! by looking for that supervisor, never by starting a third.

#![cfg(any(windows, target_os = "linux", target_os = "macos"))]

use crate::capsule::{CapsuleConfig, ExitKind, RunSignals};
use crate::lane::attach_proto::ConnId;
use crate::lane::client::{Endpoint, PlatformEndpoint};
use crate::lane::platform_transport::PlatformTransport;
use crate::lane::transport::{LaneEvent, PlatformLaneServer};
use crate::lane::wire::{self, DecodedFrame, SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply, SupervisorRequest, Survival};
#[cfg(any(test, feature = "test-support"))]
use sot_platform::identity::challenge::ChallengeOutcome;
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
use authority::{After, Shared};
use journal::pointer::{self, PointerState};

#[cfg(windows)]
type Agent = crate::capsule::producer::conpty::producer::ConptyProducer;
#[cfg(unix)]
type Agent = crate::capsule::producer::pty::PtyProducer;

#[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
type Client = <PlatformEndpoint as Endpoint>::Client;
#[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
type Process = <PlatformEndpoint as Endpoint>::Process;
type Lane = PlatformLaneServer;

/// Connections to each lane (the supervisor's own and the agent's attach lane).
const MAX_LANE_INSTANCES: u32 = 8;
/// The lane's thread wakes at least this often, for the replies the main thread owes and for its own exit.
const LANE_TICK: Duration = Duration::from_millis(50);
/// The main thread's wait between looks at what a client asked.
const MAIN_LOOP_POLL: Duration = Duration::from_millis(100);
/// A supervisor-lane connection whose client has sent nothing for this long is closed, unless the supervisor owes it
/// the deferred `end_run` reply. `pub` so a test crossing it names this value.
pub const LANE_IDLE_DEADLINE: Duration = Duration::from_secs(5);
/// Events the lane's thread takes per tick, so one busy connection cannot hold the reply it owes another.
const LANE_EVENT_QUOTA: usize = 64;
const REFUSAL_SENT_DEADLINE: Duration = Duration::from_secs(2);
const REFUSAL_FLUSH_GRACE: Duration = Duration::from_millis(250);
/// A terminal supervisor nobody stops lingers this long, serving status, and exits.
const TERMINAL_EXIT_GRACE: Duration = Duration::from_secs(2);
/// How long the exit waits for a `stop`'s reply to be delivered.
const STOP_REPLY_BOUND: Duration = Duration::from_secs(3);

/// The launcher's contract: `0` clean end, do not restart; `69` terminal, do not restart, surface it; `70` the
/// fence was already held by a live supervisor, so look for it and never start a third; anything else is a crash.
pub const EXIT_CLEAN: i32 = 0;
pub const EXIT_TERMINAL: i32 = 69;
pub const EXIT_CONTENDED: i32 = 70;

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
    let lane = match Lane::bind_supervisor(&h, MAX_LANE_INSTANCES) {
        Ok(l) => l,
        Err(e) => {
            note(format_args!("could not bind the supervisor lane: {e}"));
            return Ok(EXIT_TERMINAL);
        }
    };

    let shared = Arc::new(Shared::new(config.state_dir.clone()));
    let lane_thread = {
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("supervisor-lane".into())
            .spawn(move || authority::serve(lane, &shared))
            .map_err(crate::Error::Io)?
    };
    let code = drive(&config, &shared);
    shared.finish();
    if lane_thread.join().is_err() {
        note(format_args!("the lane's thread panicked"));
    }
    Ok(code)
}

/// The main thread's life: discover the voyage, run the agent, and after each run wait for what a client asks.
fn drive(config: &SuperviseConfig, shared: &Shared) -> i32 {
    let mut voyage = match discover_or_mint_voyage(&config.state_dir, config.mode) {
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
            run_agent(config, shared, &voyage, agent_ran)
        };
        match ended {
            RunEnd::NotStarted => return leave(shared),
            RunEnd::Ended { started } => {
                agent_ran |= started;
                shared.run_ended();
            }
            RunEnd::Failed(detail) => shared.terminal(detail),
        }
        match shared.wait_after_run() {
            After::Stop => return finish(shared),
            After::TerminalGrace => return EXIT_TERMINAL,
            After::Reset { operation_id, new_voyage } => match pointer::reset(&config.state_dir, &new_voyage) {
                Ok(()) => {
                    shared.reset_done(&operation_id, &new_voyage);
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
    /// The run is over; `started` says the agent was started at all.
    Ended { started: bool },
    Failed(String),
}

/// One run of the agent, to its end, on this thread: the kernel's parent-death signal on Linux follows the thread
/// that forks the agent, and this one lives as long as the process.
fn run_agent(config: &SuperviseConfig, shared: &Shared, voyage: &str, agent_ran: bool) -> RunEnd {
    let (kill, commands) = mpsc::channel();
    let signals = Arc::new(RunSignals::default());
    if !shared.run_begins(kill, Arc::clone(&signals)) {
        return RunEnd::NotStarted;
    }
    let capsule = CapsuleConfig {
        voyage_root: voyage_root_path(&config.state_dir, voyage),
        voyage_id: voyage.to_string(),
        screen_path: Some(config.state_dir.join("screen")),
        argv: agent_argv(config, agent_ran),
        cols: config.cols,
        rows: config.rows,
        survival: Survival::Normal,
        signals: Some(Arc::clone(&signals)),
    };
    let mut transport = PlatformTransport::new(MAX_LANE_INSTANCES);
    let result = crate::capsule::run::<Agent>(capsule, commands, &mut transport);
    drop(transport);
    match result {
        Ok(summary) => match summary.exit_kind {
            ExitKind::SpawnFailed => RunEnd::Failed("the agent could not be started".into()),
            ExitKind::ProducerExited | ExitKind::Requested => {
                note(format_args!("the agent ended: {:?} ({:?})", summary.exit_code, summary.exit_kind));
                RunEnd::Ended { started: signals.spawned_at().is_some() }
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
// Test helpers: the supervisor lane's client side
// ---------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
pub fn connect_and_challenge_with_build_for_test(h: &str, build: &str) -> crate::Result<(Client, ChallengeOutcome<Process>)> {
    let conn = PlatformEndpoint::default().connect_supervisor_unchallenged(h)?;
    let mut exchange = crate::lane::exchange::SupervisorLaneExchange::new(build.to_string());
    let outcome = PlatformEndpoint::default().challenge(&conn, &mut exchange, Instant::now() + Duration::from_secs(2));
    Ok((conn, outcome))
}

/// The lane gate is the `proto` integer, not `build`: the one way to drive a genuine proto mismatch in a test.
#[cfg(any(test, feature = "test-support"))]
pub fn connect_and_challenge_with_proto_for_test(h: &str, proto: u32) -> crate::Result<(Client, ChallengeOutcome<Process>)> {
    let conn = PlatformEndpoint::default().connect_supervisor_unchallenged(h)?;
    let mut exchange = crate::lane::exchange::SupervisorLaneExchange::with_proto_for_test(
        crate::lane::exchange::SUPERVISOR_LANE_BUILD_ID,
        proto,
    );
    let outcome = PlatformEndpoint::default().challenge(&conn, &mut exchange, Instant::now() + Duration::from_secs(2));
    Ok((conn, outcome))
}

#[cfg(any(test, feature = "test-support"))]
pub fn connect_and_challenge_for_test(h: &str) -> crate::Result<(Client, Process)> {
    crate::attach_client::supervisor_client::connect_and_challenge::<PlatformEndpoint>(
        &PlatformEndpoint::default(),
        h,
        crate::lane::exchange::SUPERVISOR_LANE_BUILD_ID,
        Instant::now() + Duration::from_secs(2),
    )
}

#[cfg(any(test, feature = "test-support"))]
pub fn request_for_test(conn: &Client, request: &SupervisorRequest, deadline: Instant) -> crate::Result<SupervisorReply> {
    crate::attach_client::supervisor_client::send_and_read(conn, request, deadline)
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

/// The voyage the pointer names, minted on a `--start` with no pointer; a `--resume` with none has nothing to resume.
fn discover_or_mint_voyage(state_dir: &Path, mode: StartMode) -> crate::Result<String> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) => Ok(id),
        PointerState::NotFound => match mode {
            StartMode::Start => {
                std::fs::create_dir_all(voyages_dir(state_dir))?;
                let id = uuid::Uuid::now_v7().to_string();
                crate::capsule::bootstrap_voyage_folder(&voyage_root_path(state_dir, &id))?;
                pointer::publish(state_dir, &id)?;
                Ok(id)
            }
            StartMode::Resume => Err(crate::Error::State("--resume with no drawer.voyage pointer at all: nothing to resume".into())),
        },
        PointerState::Corrupt => Err(crate::Error::State("drawer.voyage is corrupt".into())),
        PointerState::OtherIo(e) => Err(e.into()),
    }
}

/// Truncate `detail` to fit within [`wire::MAX_SUPERVISOR_STRING_LEN`] bytes, on a UTF-8 boundary.
fn bounded_detail(detail: impl Into<String>) -> String {
    let mut s = detail.into();
    if s.len() > wire::MAX_SUPERVISOR_STRING_LEN {
        let mut cut = wire::MAX_SUPERVISOR_STRING_LEN;
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
