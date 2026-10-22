//! The supervisor lane: what a client may ask of the one process of a row, and the state the main thread and the lane's
//! thread share. The lane's own thread decodes, answers and records; the main thread owns the agent and tells the
//! lane what became of it (`Shared`'s `run_*` methods).

use super::*;
use crate::capsule::{Command, RunSignals};

/// A `starting` supervisor reads `ready` this long after its agent started, if the agent has not written before.
const READY_AFTER: Duration = Duration::from_secs(2);

/// A client's connection owed a reply the supervisor has not yet got to: the `end_run` that waits on the run's end.
struct Ending {
    operation_id: String,
    conn: ConnId,
}

/// What a client was told about an operation, kept in memory for its resubmits and queries: the operation's
/// kind and the state it reached.
struct Operation {
    op: SupervisorOp,
    state: SupervisorOperationState,
}

/// The agent's run in progress: the channel that ends it, and what it tells its owner.
struct LiveRun {
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
    operations: HashMap<String, Operation>,
    run: Option<LiveRun>,
    ending: Option<Ending>,
    /// A reply the lane's thread owes a connection, handed over by the main thread when the run ended.
    owed_reply: Option<(ConnId, SupervisorOperationState)>,
    reset: Option<(String, String)>,
    stop: Option<StopRequest>,
    terminal_since: Option<Instant>,
    /// The main thread is done; the lane's thread ends.
    exiting: bool,
}

pub(super) struct Shared {
    self_pid: u32,
    self_created: u64,
    state_dir: PathBuf,
    inner: Mutex<Inner>,
    changed: Condvar,
}

/// What the main thread does once the agent's run ended.
pub(super) enum After {
    Stop,
    Reset { operation_id: String, new_voyage: String },
    /// A terminal supervisor with no stop asked lingers `TERMINAL_EXIT_GRACE` and exits.
    TerminalGrace,
}

impl Shared {
    pub(super) fn new(state_dir: PathBuf) -> Shared {
        let (self_pid, self_created) = self_pid_and_created().unwrap_or((0, 0));
        Shared {
            self_pid,
            self_created,
            state_dir,
            inner: Mutex::new(Inner {
                phase: SupervisorPhase::Starting,
                voyage: None,
                detail: None,
                operations: HashMap::new(),
                run: None,
                ending: None,
                owed_reply: None,
                reset: None,
                stop: None,
                terminal_since: None,
                exiting: false,
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

    /// A run begins: its kill channel and signals are what `end_run`, `stop` and status read. `false` when a stop was
    /// asked before the agent started: nothing is started.
    pub(super) fn run_begins(&self, kill: mpsc::Sender<Command>, signals: Arc<RunSignals>) -> bool {
        let mut inner = self.lock();
        if inner.stop.is_some() {
            return false;
        }
        inner.phase = SupervisorPhase::Starting;
        inner.run = Some(LiveRun { kill, signals });
        true
    }

    /// The run ended with the agent gone: the supervisor rests until a reset or a stop.
    pub(super) fn run_ended(&self) {
        let mut inner = self.lock();
        inner.run = None;
        inner.phase = SupervisorPhase::EndedNoRespawn;
        if let Some(ending) = inner.ending.take() {
            let state = SupervisorOperationState::RecordVerified;
            if let Some(op) = inner.operations.get_mut(&ending.operation_id) {
                op.state = state.clone();
            }
            inner.owed_reply = Some((ending.conn, state));
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
        if let Some(ending) = inner.ending.take() {
            let state = SupervisorOperationState::Failed { detail: bounded_detail(detail.clone()) };
            if let Some(op) = inner.operations.get_mut(&ending.operation_id) {
                op.state = state.clone();
            }
            inner.owed_reply = Some((ending.conn, state));
        }
        if let Some((operation_id, _)) = inner.reset.take() {
            if let Some(op) = inner.operations.get_mut(&operation_id) {
                op.state = SupervisorOperationState::Failed { detail: bounded_detail(detail.clone()) };
            }
        }
        inner.detail = Some(detail);
        self.changed.notify_all();
    }

    /// The reset's new voyage is published and its folder is there: the next run starts under it.
    pub(super) fn reset_done(&self, operation_id: &str, new_voyage: &str) {
        let mut inner = self.lock();
        inner.voyage = Some(new_voyage.to_string());
        if let Some(op) = inner.operations.get_mut(operation_id) {
            op.state = SupervisorOperationState::ResetDone { new_voyage: new_voyage.to_string() };
        }
    }

    /// Blocks until the supervisor has something to do after the run: a stop, a reset, or the terminal grace over.
    pub(super) fn wait_after_run(&self) -> After {
        let mut inner = self.lock();
        loop {
            if inner.stop.is_some() {
                return After::Stop;
            }
            if let Some((operation_id, new_voyage)) = inner.reset.take() {
                return After::Reset { operation_id, new_voyage };
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

    /// An `end_run` is in flight (a stop is not one).
    pub(super) fn end_run_asked(&self) -> bool {
        self.lock().ending.is_some()
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

    /// The lane's thread is no longer needed.
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

    fn status(&self) -> SupervisorReply {
        let inner = self.lock();
        let phase = Self::wire_phase(&inner);
        let leg = match (&inner.voyage, phase) {
            (Some(v), SupervisorPhase::Ready | SupervisorPhase::Ending) => {
                crate::capsule::read_leg_epoch(&voyage_root_path(&self.state_dir, v))
            }
            _ => None,
        };
        SupervisorReply::StatusOk {
            pid: self.self_pid,
            created: self.self_created,
            voyage: inner.voyage.clone(),
            leg,
            phase,
        }
    }

    fn query(&self, operation_id: &str) -> SupervisorOperationState {
        self.lock()
            .operations
            .get(operation_id)
            .map_or(SupervisorOperationState::UnknownOperation, |o| o.state.clone())
    }

    /// Admits or answers a command. `Err` is the reply to send now; `Ok(None)` is an admitted `end_run`, whose reply
    /// is owed until the run ends; `Ok(Some(_))` is an admitted reset or stop with its reply.
    fn command(
        &self,
        conn: ConnId,
        operation_id: String,
        op: SupervisorOp,
    ) -> Result<Option<SupervisorOperationState>, SupervisorOperationState> {
        let mut inner = self.lock();
        // An id already seen answers from its record, before any fencing: a resubmit never runs twice.
        if let Some(seen) = inner.operations.get(&operation_id) {
            if seen.op != op {
                return Err(SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::IdConflict });
            }
            return Err(seen.state.clone());
        }
        let fenced_ok = match &op {
            SupervisorOp::EndRun { voyage, .. } => inner.voyage.as_deref() == Some(voyage.as_str()),
            SupervisorOp::Reset { voyage: Some(v) } => inner.voyage.as_deref() == Some(v.as_str()),
            SupervisorOp::Reset { voyage: None } => inner.voyage.is_none(),
            SupervisorOp::Stop => true,
        };
        if !fenced_ok {
            return Err(SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::StaleVoyage });
        }
        let failed = |why: &str| SupervisorOperationState::Failed { detail: bounded_detail(why) };
        match &op {
            SupervisorOp::EndRun { .. } => {
                let live = inner.phase == SupervisorPhase::Starting && inner.run.is_some();
                if !live {
                    return Err(failed(match inner.phase {
                        SupervisorPhase::Ending => "an end_run is currently in progress",
                        SupervisorPhase::Terminal => "the supervisor is in a terminal state",
                        _ => "no run is currently live",
                    }));
                }
                inner.phase = SupervisorPhase::Ending;
                inner.ending = Some(Ending { operation_id: operation_id.clone(), conn });
                if let Some(run) = &inner.run {
                    let kill = run.kill.clone();
                    // Test seam: the run learns of the request this many milliseconds after it was admitted, so a test
                    // can end the agent in between. Inert unless named.
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
                }
                inner.operations.insert(operation_id, Operation { op, state: SupervisorOperationState::Accepted });
                Ok(None)
            }
            SupervisorOp::Reset { .. } => {
                if inner.phase != SupervisorPhase::EndedNoRespawn || inner.reset.is_some() {
                    return Err(failed(match inner.phase {
                        SupervisorPhase::Starting => "a run is currently live; end the run before resetting",
                        SupervisorPhase::Ending => "an end_run is currently in progress",
                        SupervisorPhase::Terminal => "the supervisor is in a terminal state",
                        _ => "a reset is already in progress",
                    }));
                }
                let new_voyage = uuid::Uuid::now_v7().to_string();
                inner.reset = Some((operation_id.clone(), new_voyage));
                inner.phase = SupervisorPhase::Starting;
                inner.operations.insert(operation_id, Operation { op, state: SupervisorOperationState::Accepted });
                self.changed.notify_all();
                Ok(Some(SupervisorOperationState::Accepted))
            }
            SupervisorOp::Stop => {
                if inner.stop.is_none() {
                    inner.stop = Some(StopRequest { primary_conn: conn, delivered: false });
                    if let Some(run) = &inner.run {
                        let _ = run.kill.send(Command::Stop);
                    }
                }
                inner.operations.insert(operation_id, Operation { op, state: SupervisorOperationState::Stopping });
                self.changed.notify_all();
                Ok(Some(SupervisorOperationState::Stopping))
            }
        }
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

    fn owed(&self) -> (Option<ConnId>, Option<(ConnId, SupervisorOperationState)>) {
        let mut inner = self.lock();
        let owed_conn = inner.ending.as_ref().map(|e| e.conn);
        (owed_conn, inner.owed_reply.take())
    }

    fn exiting(&self) -> bool {
        self.lock().exiting
    }
}

// ---------------------------------------------------------------------
// The lane's thread
// ---------------------------------------------------------------------

/// A refusal (or `stop`) reply queued as a connection's last word waits through two stages before the connection
/// closes: first for `LaneEvent::Sent` (the write completed) or a bounded deadline, then a flush grace so the
/// client's read has a chance to drain the bytes.
enum PendingClose {
    AwaitingSent { deadline: Instant },
    FlushGrace { close_at: Instant },
}

struct Conn {
    splitter: wire::FrameSplitter,
    hello_ok: bool,
    last_activity: Instant,
    pending_close: Option<PendingClose>,
}

/// Serves the lane until the main thread is done. The lane drops here, before the fence does.
pub(super) fn serve(lane: Lane, shared: &Shared) {
    let mut conns: HashMap<ConnId, Conn> = HashMap::new();
    let mut woke_on: Option<LaneEvent> = None;
    loop {
        let now = Instant::now();
        if service_lane(&lane, woke_on.take(), &mut conns, shared, now) {
            shared.terminal("supervisor lane accept loop failed permanently".into());
        }
        let (owed_conn, owed_reply) = shared.owed();
        if let Some((id, state)) = owed_reply {
            if let Some(conn) = conns.get_mut(&id) {
                conn.last_activity = now;
                let _ = lane.send(id, encode_reply_or_fallback(&SupervisorReply::Operation(state)), None);
            }
        }
        close_due(&lane, &mut conns, shared, owed_conn, now);
        if shared.exiting() {
            break;
        }
        woke_on = lane.events().recv_timeout(LANE_TICK).ok();
    }
}

/// Drains up to `LANE_EVENT_QUOTA` events. `true` iff the accept loop died for good.
fn service_lane(lane: &Lane, mut first: Option<LaneEvent>, conns: &mut HashMap<ConnId, Conn>, shared: &Shared, now: Instant) -> bool {
    let mut accept_loop_dead = false;
    for _ in 0..LANE_EVENT_QUOTA {
        let event = match first.take() {
            Some(ev) => ev,
            None => match lane.events().try_recv() {
                Ok(ev) => ev,
                Err(_) => break,
            },
        };
        match event {
            LaneEvent::Accepted(id) => {
                conns.insert(
                    id,
                    Conn { splitter: wire::FrameSplitter::new(), hello_ok: false, last_activity: now, pending_close: None },
                );
            }
            LaneEvent::Bytes(id, bytes) => handle_lane_bytes(lane, conns, id, &bytes, shared, now),
            LaneEvent::Closed(id, _reason) => {
                conns.remove(&id);
                shared.conn_gone(id);
            }
            LaneEvent::Sent(id, _marker) => {
                if let Some(conn) = conns.get_mut(&id) {
                    if matches!(conn.pending_close, Some(PendingClose::AwaitingSent { .. })) {
                        conn.pending_close = Some(PendingClose::FlushGrace { close_at: now + REFUSAL_FLUSH_GRACE });
                    }
                }
            }
            LaneEvent::AcceptError(e) => {
                note(format_args!("supervisor lane accept loop failed permanently: {e}"));
                accept_loop_dead = true;
            }
        }
    }
    accept_loop_dead
}

/// Closes what is due: a connection whose last word was delivered or given up on, and one idle past
/// `LANE_IDLE_DEADLINE` that is not owed the deferred `end_run` reply.
fn close_due(lane: &Lane, conns: &mut HashMap<ConnId, Conn>, shared: &Shared, owed_conn: Option<ConnId>, now: Instant) {
    let mut to_close: Vec<ConnId> = Vec::new();
    for (id, conn) in conns.iter() {
        let idle = now.saturating_duration_since(conn.last_activity) >= LANE_IDLE_DEADLINE;
        let due = match &conn.pending_close {
            Some(PendingClose::AwaitingSent { deadline }) => {
                if now >= *deadline {
                    note(format_args!("a reply was never confirmed sent; closing anyway"));
                }
                now >= *deadline
            }
            Some(PendingClose::FlushGrace { close_at }) => now >= *close_at,
            None => idle && owed_conn != Some(*id),
        };
        if due {
            to_close.push(*id);
        }
    }
    for id in to_close {
        lane.close(id);
        conns.remove(&id);
        shared.conn_gone(id);
    }
}

fn encode_reply_or_fallback(reply: &SupervisorReply) -> Vec<u8> {
    wire::encode_supervisor_reply(reply).unwrap_or_else(|e| {
        note(format_args!("a reply failed to encode ({e}); substituting a minimal failure reply"));
        wire::encode_supervisor_reply(&SupervisorReply::Operation(SupervisorOperationState::Failed {
            detail: "internal error".into(),
        }))
        .expect("this minimal fallback reply is always encodable")
    })
}

/// Feeds one connection's bytes through its splitter and answers each frame.
fn handle_lane_bytes(lane: &Lane, conns: &mut HashMap<ConnId, Conn>, id: ConnId, bytes: &[u8], shared: &Shared, now: Instant) {
    let mut close_after = false;
    let mut pending: Option<PendingClose> = None;
    {
        let Some(conn) = conns.get_mut(&id) else { return };
        conn.last_activity = now;
        let (frames, err) = conn.splitter.feed(bytes);
        for frame in frames {
            match frame {
                DecodedFrame::SupervisorRequest(SupervisorRequest::Hello { proto, build: _ }) if !conn.hello_ok => {
                    // The gate is the protocol integer alone; `build` rides the wire as information.
                    if proto != wire::SUPERVISOR_PROTO_V1 {
                        note(format_args!("hello refused: lane proto {proto}, ours {}", wire::SUPERVISOR_PROTO_V1));
                        let reply = wire::encode_supervisor_reply(&SupervisorReply::Refused {
                            reason: wire::SupervisorRefusedReason::VersionSkew,
                        })
                        .expect("Refused encodes unconditionally");
                        match lane.send(id, reply, Some(id)) {
                            Ok(()) => pending = Some(PendingClose::AwaitingSent { deadline: now + REFUSAL_SENT_DEADLINE }),
                            Err(_) => close_after = true,
                        }
                        break;
                    }
                    conn.hello_ok = true;
                    let reply = wire::encode_supervisor_reply(&SupervisorReply::HelloOk {
                        proto: wire::SUPERVISOR_PROTO_V1,
                        build: crate::lane::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
                        pid: shared.self_pid,
                        created: shared.self_created,
                    })
                    .expect("HelloOk's build is this crate's own bounded constant");
                    let _ = lane.send(id, reply, None);
                }
                DecodedFrame::SupervisorRequest(SupervisorRequest::Hello { .. }) => {
                    close_after = true;
                    break;
                }
                DecodedFrame::SupervisorRequest(_) if !conn.hello_ok => {
                    close_after = true;
                    break;
                }
                DecodedFrame::SupervisorRequest(SupervisorRequest::Status) => {
                    let _ = lane.send(id, encode_reply_or_fallback(&shared.status()), None);
                }
                DecodedFrame::SupervisorRequest(SupervisorRequest::Query { operation_id }) => {
                    let reply = SupervisorReply::Operation(shared.query(&operation_id));
                    let _ = lane.send(id, encode_reply_or_fallback(&reply), None);
                }
                DecodedFrame::SupervisorRequest(SupervisorRequest::Command { operation_id, op }) => {
                    let is_stop = matches!(op, SupervisorOp::Stop);
                    match shared.command(id, operation_id, op) {
                        // An admitted `end_run`: the reply is owed until the run ends.
                        Ok(None) => {}
                        Ok(Some(state)) if is_stop => {
                            // A stop's reply is the connection's last word: delivered, then closed.
                            let reply = encode_reply_or_fallback(&SupervisorReply::Operation(state));
                            match lane.send(id, reply, Some(id)) {
                                Ok(()) => pending = Some(PendingClose::AwaitingSent { deadline: now + REFUSAL_SENT_DEADLINE }),
                                Err(_) => close_after = true,
                            }
                        }
                        Ok(Some(state)) | Err(state) => {
                            let reply = encode_reply_or_fallback(&SupervisorReply::Operation(state));
                            let _ = lane.send(id, reply, None);
                        }
                    }
                }
                _ => {
                    close_after = true;
                    break;
                }
            }
            if close_after {
                break;
            }
        }
        if err.is_some() {
            close_after = true;
        }
        if let Some(p) = pending {
            conn.pending_close = Some(p);
        }
    }
    if close_after {
        lane.close(id);
        conns.remove(&id);
        shared.conn_gone(id);
    }
}

#[cfg(windows)]
pub(super) fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
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
pub(super) fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    Ok((std::process::id(), sot_platform::identity::challenge_unix::self_start_ticks()?))
}

/// On macOS the identity is the kernel's `pidversion`, the value the challenge reads out of the peer's audit token;
/// a start time here would make every challenge `Foreign`.
#[cfg(target_os = "macos")]
pub(super) fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    Ok((std::process::id(), u64::from(sot_platform::identity::challenge_macos::self_pidversion()?)))
}

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;
