#![cfg(any(windows, target_os = "linux", target_os = "macos"))]
//! The daemon's calls on one row's supervisor: `status`, `end_run`, `reset` and `stop`, each one connection on the row
//! socket (`row-<h>`): the row client's dial, hello and challenge, then one request with its bound. The exception is
//! [`Persistent`], which holds one connection across status calls and reconnects when it fails. A call that loses its
//! reply is answered by reading status again; no call carries an id.
//!
//! The dial checks the row number the hello carries and never the frames' wire integer, so a daemon update never
//! strands a live row: a row of another row number is [`crate::Error::VersionSkew`], which rows reads as `foreign`.

use crate::lane::client::{Endpoint, PlatformEndpoint};
use crate::lane::connect_own::connect_own;
use crate::SupervisorPhase;
use crate::row::{
    row_socket_path, EndRunOutcome as RowEndRun, EndRunRes, ResetOutcome, ResetRes, RowCanceler, RowClient, RowError, StatusRes,
    StopRes, OP_END_RUN, OP_RESET, OP_STATUS, OP_STOP,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the dial and the hello may take.
const HELLO_BUDGET: Duration = Duration::from_secs(2);
/// How long a status reply may take.
const STATUS_BUDGET: Duration = Duration::from_secs(5);
/// How long an `end_run`'s reply may take; the supervisor answers `ending` before this passes.
const END_RUN_BUDGET: Duration = Duration::from_secs(30);
/// How long a `reset`'s reply may take; the supervisor answers `failed` before this passes.
const RESET_BUDGET: Duration = Duration::from_secs(30);
/// How long a `stop` may take in all: its reply, then the supervisor's exit.
const STOP_BUDGET: Duration = Duration::from_secs(30);

/// The retained-process type the calls return: [`PlatformEndpoint`]'s own `Process`, so a caller outside this crate can
/// name it without depending on the platform's challenge module.
pub type ChallengedProcess = <PlatformEndpoint as Endpoint>::Process;

/// What a `status` round trip reports. `pid` and `created` are the reporting supervisor's own identity, off the hello.
#[derive(Debug, Clone)]
pub struct StatusReport {
    pub pid: u32,
    pub created: u64,
    pub phase: SupervisorPhase,
    /// The live run's id, absent while none is live.
    pub run: Option<String>,
    pub voyage: Option<String>,
}

/// What [`end_run`] settled on.
#[derive(Debug, Clone)]
pub enum EndRunOutcome {
    /// The run ended and its requested end is recorded.
    Ended,
    /// The supervisor reports the end failed: no run was live, one was already ending, it could not be recorded, or the
    /// supervisor is terminal.
    Failed(String),
    /// The run named is not the current run: nothing changed.
    Refused(String),
    /// No answer that the run had ended: the supervisor said it had not ended within its bound, or the connection
    /// failed. The caller reads status.
    OutcomeUnknown,
}

fn err_state(msg: impl Into<String>) -> crate::Error {
    crate::Error::State(msg.into())
}

fn row_error(e: RowError) -> crate::Error {
    match e {
        RowError::RowMismatch { .. } => crate::Error::VersionSkew,
        RowError::Io(e) => crate::Error::Io(e),
        other => err_state(format!("the row socket: {other}")),
    }
}

/// Dials the row socket of `state_dir`, proves the peer and checks its row number, all within `bound`.
fn connect(state_dir: &Path, bound: Duration) -> crate::Result<(RowClient, ChallengedProcess)> {
    let deadline = Instant::now() + bound;
    let h = sot_platform::host::state_dir::state_dir_hash(state_dir);
    let path = row_socket_path(&h).map_err(row_error)?;
    // A failed dial stays a transport error, so a caller can tell "nothing listens" from a peer that answered.
    let conn = connect_own(&path)?;
    RowClient::over_proven(&PlatformEndpoint::default(), conn, crate::BUILD_ID, deadline)
        .map_err(row_error)
}

/// One request and its reply decoded.
fn request<T: DeserializeOwned>(client: &mut RowClient, op: &str, payload: serde_json::Value, bound: Duration) -> crate::Result<T> {
    let frame = client.call(op, payload, bound).map_err(row_error)?;
    serde_json::from_value(frame.payload).map_err(|e| err_state(format!("{op}: a reply that is not what was asked: {e}")))
}

fn phase_of(text: &str) -> crate::Result<SupervisorPhase> {
    Ok(match text {
        "starting" => SupervisorPhase::Starting,
        "ready" => SupervisorPhase::Ready,
        "ending" => SupervisorPhase::Ending,
        "ended_no_respawn" => SupervisorPhase::EndedNoRespawn,
        "terminal" => SupervisorPhase::Terminal,
        other => return Err(err_state(format!("status: a phase this client does not know: {other}"))),
    })
}

fn report(status: StatusRes, process_identity: (u32, u64)) -> crate::Result<StatusReport> {
    Ok(StatusReport {
        pid: process_identity.0,
        created: process_identity.1,
        phase: phase_of(&status.phase)?,
        run: status.run,
        voyage: status.voyage,
    })
}

/// Connect, challenge, and run one `status` request: everything a caller needs to map a row's supervisor to the
/// `phase` `workspace.list` reports. Any failure (connect refused, the challenge proving `Foreign` or `Undetermined`, a
/// timeout, a malformed reply) is one `Err`, except a row of another row number, which is `Error::VersionSkew`.
///
/// Returns the [`ChallengedProcess`] alongside the status: the challenge already proves and retains a live handle to
/// the process on the other end, and a caller that adopted a row needs exactly that as its death signal. A caller with
/// no use for it drops the second element, which closes the handle.
pub fn query_status(state_dir: &Path) -> crate::Result<(StatusReport, ChallengedProcess)> {
    let (mut client, process) = connect(state_dir, HELLO_BUDGET)?;
    let status: StatusRes = request(&mut client, OP_STATUS, json!({}), STATUS_BUDGET)?;
    Ok((report(status, client.peer())?, process))
}

/// Connect, challenge, and send `stop`: the supervisor ends its agent and exits once the reply's connection has ended.
///
/// Waits for confirmed process death after the reply: a caller that acted on "stopped" the instant the reply came
/// (starting a fresh supervisor, say) could race the old one's own teardown. The connection is dropped first, then this
/// blocks on the retained [`ChallengedProcess`], all within [`STOP_BUDGET`]. A reply that never came does not end the
/// stop: the supervisor's exit, waited for the same way, is the outcome.
pub fn stop(state_dir: &Path) -> crate::Result<()> {
    let deadline = Instant::now() + STOP_BUDGET;
    let (mut client, process) = connect(state_dir, HELLO_BUDGET)?;
    let left = deadline.saturating_duration_since(Instant::now());
    // A refusal is an answer: nothing is stopping, so there is no exit to wait for. A reply that never came is not.
    let unanswered = match client.call(OP_STOP, json!({}), left) {
        Ok(frame) => match serde_json::from_value::<StopRes>(frame.payload) {
            Ok(_) => None,
            Err(e) => return Err(err_state(format!("stop: a reply that is not what was asked: {e}"))),
        },
        Err(RowError::Refused(msg)) => return Err(err_state(format!("stop was refused: {msg}"))),
        Err(e) => Some(row_error(e)),
    };
    drop(client);
    let left = deadline.saturating_duration_since(Instant::now());
    match process.wait(left) {
        Ok(true) => Ok(()),
        Ok(false) => Err(err_state(match unanswered {
            None => format!("supervisor acknowledged stop but did not exit within {STOP_BUDGET:?}"),
            Some(e) => format!("stop got no reply ({e}) and the supervisor did not exit within {STOP_BUDGET:?}"),
        })),
        Err(e) => Err(err_state(format!("waiting for the stopped supervisor to exit: {e}"))),
    }
}

/// How long a lost or unfinished answer is settled by reading status, from the answer's loss on.
const RECOVER_BUDGET: Duration = Duration::from_secs(10);
const RECOVER_POLL: Duration = Duration::from_millis(250);

/// Status from the supervisor `peer` names: a fresh dial, read again until `settled` says what it means or the budget
/// passes. A dial that reaches another process, or none, settles nothing: the answer was the original supervisor's,
/// so a replacement's status never stands for it.
fn recover<T>(state_dir: &Path, peer: (u32, u64), mut settled: impl FnMut(StatusRes) -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + RECOVER_BUDGET;
    loop {
        if let Ok((mut client, _process)) = connect(state_dir, HELLO_BUDGET) {
            if client.peer() != peer {
                return None;
            }
            if let Ok(status) = request::<StatusRes>(&mut client, OP_STATUS, json!({}), STATUS_BUDGET) {
                if let Some(answer) = settled(status) {
                    return Some(answer);
                }
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(RECOVER_POLL);
    }
}

/// Connect, challenge, and ask the supervisor to end the run `run` names. `run` is the run the caller most recently
/// read from [`query_status`]: the end is fenced by it, so another run's id is refused and nothing changes.
///
/// An answer that was lost, or the supervisor's `ending` (the run had not ended in its bound), is settled by reading
/// status from the same supervisor: the run gone means it ended (or, with the supervisor terminal, that the end failed),
/// the run still there means it did not. No end is asked a second time, and a replacement run's status never stands
/// for this one: what is still unsettled is [`EndRunOutcome::OutcomeUnknown`].
pub fn end_run(state_dir: &Path, run: &str, reason: &str) -> crate::Result<EndRunOutcome> {
    let (mut client, _process) = connect(state_dir, HELLO_BUDGET)?;
    let peer = client.peer();
    let reply: crate::Result<EndRunRes> = request(&mut client, OP_END_RUN, json!({"run": run, "reason": reason}), END_RUN_BUDGET);
    drop(client);
    if let Ok(res) = &reply {
        match res.outcome {
            RowEndRun::Ended => return Ok(EndRunOutcome::Ended),
            RowEndRun::StaleRun => return Ok(EndRunOutcome::Refused("stale_run: the run named is not the current run".into())),
            RowEndRun::Failed => return Ok(EndRunOutcome::Failed(res.detail.clone().unwrap_or_default())),
            RowEndRun::Ending => {}
        }
    }
    Ok(recover(state_dir, peer, |status| match (status.run.as_deref() == Some(run), phase_of(&status.phase)) {
        (false, Ok(SupervisorPhase::Terminal)) => Some(EndRunOutcome::Failed("the supervisor went terminal".into())),
        (false, Ok(_)) => Some(EndRunOutcome::Ended),
        // The run is there: while its end is in progress, wait for it; with none in progress, none was admitted.
        (true, Ok(SupervisorPhase::Ending)) => None,
        (true, _) => Some(EndRunOutcome::OutcomeUnknown),
        (_, Err(_)) => None,
    })
    .unwrap_or(EndRunOutcome::OutcomeUnknown))
}

/// Connect, challenge, and send `reset`: the one operation a supervisor resting in `ended_no_respawn` admits. It mints
/// a fresh voyage and the next run starts under it, the deliberate alternative to `--resume`, which never resurrects a
/// voyage whose run was asked to end. Returns the new voyage id.
///
/// A reply that was lost is settled by reading status from the same supervisor: a voyage other than the one it was
/// resting on is the reset's, published; the same voyage, with the supervisor still resting or terminal, is not.
pub fn reset(state_dir: &Path) -> crate::Result<String> {
    let (mut client, _process) = connect(state_dir, HELLO_BUDGET)?;
    let peer = client.peer();
    let before: StatusRes = request(&mut client, OP_STATUS, json!({}), STATUS_BUDGET)?;
    let reply: crate::Result<ResetRes> = request(&mut client, OP_RESET, json!({}), RESET_BUDGET);
    drop(client);
    let lost = match reply {
        Ok(res) => {
            return match res.outcome {
                ResetOutcome::Reset => res.voyage.ok_or_else(|| err_state("reset: the reply names no voyage")),
                ResetOutcome::Failed => Err(err_state(format!("reset did not complete: {}", res.detail.unwrap_or_default()))),
            }
        }
        Err(e) => e,
    };
    let settled = recover(state_dir, peer, |status| match (&status.voyage, phase_of(&status.phase)) {
        (Some(now), Ok(_)) if Some(now) != before.voyage.as_ref() => Some(Ok(now.clone())),
        (_, Ok(SupervisorPhase::Terminal | SupervisorPhase::EndedNoRespawn)) => Some(Err(())),
        _ => None,
    });
    match settled {
        Some(Ok(voyage)) => Ok(voyage),
        _ => Err(err_state(format!("reset was not answered and no new voyage was found published: {lost}"))),
    }
}

/// [`Persistent`]'s way to be interrupted, and whether it was.
struct Shared {
    canceler: Option<RowCanceler>,
    /// Latched by [`CancelHandle::cancel`]; never cleared, so a cancelled client never reconnects.
    cancelled: bool,
}

/// A held connection to one row, reused across [`Persistent::status`] calls. It holds one of the row's connections.
pub struct Persistent {
    state_dir: PathBuf,
    conn: Option<RowClient>,
    /// Lets [`Persistent::cancel_handle`] reach the current connection from another thread.
    shared: Arc<Mutex<Shared>>,
    /// Test-support only: fires between connect and publish, to land a cancel deterministically.
    #[cfg(any(test, feature = "test-support"))]
    after_connect_before_publish: Option<Box<dyn FnMut() + Send>>,
}

#[derive(Clone)]
pub struct CancelHandle(Arc<Mutex<Shared>>);

impl CancelHandle {
    /// Sets `cancelled` and takes and cancels the published connection under the same lock, so a concurrent
    /// [`Persistent::publish`] either installs first (and gets cancelled here) or sees `cancelled` and never installs.
    pub fn cancel(&self) {
        let mut shared = self.0.lock().unwrap_or_else(|e| e.into_inner());
        shared.cancelled = true;
        if let Some(canceler) = shared.canceler.take() {
            canceler.cancel();
        }
    }
}

impl Persistent {
    pub fn new(state_dir: &Path) -> Self {
        Persistent {
            state_dir: state_dir.to_path_buf(),
            conn: None,
            shared: Arc::new(Mutex::new(Shared { canceler: None, cancelled: false })),
            #[cfg(any(test, feature = "test-support"))]
            after_connect_before_publish: None,
        }
    }

    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle(self.shared.clone())
    }

    /// Test-support only: runs `hook` once, synchronously, between connect and publish.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_hook_after_connect_before_publish(&mut self, hook: impl FnMut() + Send + 'static) {
        self.after_connect_before_publish = Some(Box::new(hook));
    }

    fn is_cancelled(&self) -> bool {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).cancelled
    }

    /// One `status` round; retried once on a fresh connection if the held one fails.
    pub fn status(&mut self) -> crate::Result<StatusReport> {
        if self.is_cancelled() {
            return Err(err_state("cancelled".to_string()));
        }
        if self.conn.is_none() {
            let fresh = self.fresh_connection()?;
            self.publish(fresh)?;
        }
        match self.status_on_held_connection() {
            Ok(report) => Ok(report),
            Err(_first_attempt_err) => {
                // Presume the held connection is dead; a second failure is reported as-is, below.
                self.clear_conn();
                let fresh = self.fresh_connection()?;
                self.publish(fresh)?;
                let result = self.status_on_held_connection();
                if result.is_err() {
                    self.clear_conn();
                }
                result
            }
        }
    }

    /// Discards the held connection without touching `cancelled`: never a cancellation path.
    fn clear_conn(&mut self) {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).canceler = None;
        self.conn = None;
    }

    /// Installs a fresh client as held and published, unless `cancelled` is already set under the same lock, in which
    /// case the connection is cancelled and discarded: the one gate a connect becomes visible through.
    fn publish(&mut self, fresh: (RowClient, ChallengedProcess)) -> crate::Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(hook) = self.after_connect_before_publish.as_mut() {
            hook();
        }
        // The process handle is dropped: a held status connection has no use for the row's death signal.
        let (client, _process) = fresh;
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if shared.cancelled {
            drop(shared);
            client.canceler().cancel();
            return Err(err_state("cancelled".to_string()));
        }
        shared.canceler = Some(client.canceler());
        drop(shared);
        self.conn = Some(client);
        Ok(())
    }

    fn fresh_connection(&self) -> crate::Result<(RowClient, ChallengedProcess)> {
        connect(&self.state_dir, HELLO_BUDGET)
    }

    fn status_on_held_connection(&mut self) -> crate::Result<StatusReport> {
        let client = self.conn.as_mut().expect("status_on_held_connection: caller just ensured a connection");
        let status: StatusRes = request(client, OP_STATUS, json!({}), STATUS_BUDGET)?;
        report(status, client.peer())
    }
}
