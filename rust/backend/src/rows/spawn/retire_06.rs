//! Retiring a row a 0.6 release left (deleted whole in the release after 0.7.0).
//!
//! A 0.6 `stop` ends only the supervisor: its terminal (the leg, `sot-capsule run`) and the agent under it live on by
//! design. A 0.7 start over such a folder would run a second agent beside the old one, so before a row is started or
//! ended its old run is asked to end (`end_run` on the voyage its status names), its supervisor is stopped, and the 0.6
//! files are moved into `.retired-voyages/`, which the start sweep removes a day later. An end the person asked for under
//! 0.6 is kept as a 0.7 `ended` record, so the row rests until selected; every other row starts fresh.
//!
//! The 0.6 supervisor lane is restated here (little-endian), so this file needs no 0.6 code. A frame is `SOSV`, a `u32`
//! body length (at most 1 MiB) and the body. A string is a `u8` length (at most 128; an operation id at most 64) and its
//! UTF-8 bytes; a flag is one byte, 0 or 1. Requests: Hello `01` proto build; Command `02` operation-id op, where EndRun is
//! `01` reason voyage and Stop is `03`; Status `03`. Replies: HelloOk `81` proto build pid created; Refused `82` reason;
//! Operation `83` state (`01` accepted, `03` record closed, `04` record verified, `05` reset done and a string, `06`
//! stopping, `07` failed and a string, `08` refused and a reason, `09` unknown); StatusOk `84` pid created, a flag and a
//! voyage, a flag and a `u64` leg, a phase (0 starting, 1 ready, 2 ending, 3 ended without respawn, 4 terminal). The
//! endpoint is `<runtime dir>/supervisor-<h>.sock` (the pipe `\\.\pipe\sot-supervisor-<h>` on Windows), `h` being the
//! state folder's hash. The frozen bytes are the two fixtures beside this file.

use crate::rows::run::end_run::{absence_proof_then, row_socket_silent, NotProven};
use sot_capsule::endpoint::{Endpoint, PlatformEndpoint};
use sot_platform::transport::connect_own::connect_own;
use sot_platform::identity::challenge::ChallengeOutcome;
use sot_platform::identity::deadline::run_with_deadline;
use sot_platform::identity::exchange::{ExchangeDecode, IdentityExchange};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where the retired files go, inside the row's state folder.
pub(crate) const RETIRED_DIR: &str = ".retired-voyages";

/// The names a 0.6 release left in a state folder (besides `drawer.voyage.` and its leftovers).
const LAYOUT_NAMES: [&str; 4] = ["voyages", "drawer.voyage", "supervisor-journal", "rollout-evidence.json"];

/// The challenge's reply bound (0.6.6's own).
const CHALLENGE_BOUND: Duration = Duration::from_secs(2);
/// One `status` reply's bound.
const STATUS_BOUND: Duration = Duration::from_secs(5);
/// The shared budget for waiting out a starting or ending supervisor and the end of the run (0.6.6's `QUIT_CUTOFF`).
const QUIT_CUTOFF: Duration = Duration::from_secs(90);
/// The pause between two `status` asks while the supervisor is starting or ending; it also keeps 0.6's idle close away.
const SETTLE_PAUSE: Duration = Duration::from_secs(1);
/// The `stop` reply's bound; a missing reply is logged, never fatal.
const STOP_REPLY_BOUND: Duration = Duration::from_secs(5);
/// How long a stopped supervisor has to exit (0.6.6's `TEARDOWN_AGGREGATE_DEADLINE`).
const EXIT_BOUND: Duration = Duration::from_secs(20);
/// How long a terminated one has (0.6.6's `KILL_WAIT_BOUND`).
const KILL_WAIT_BOUND: Duration = Duration::from_secs(10);
/// The operation ids this retire sends carry this prefix, which the journal read excludes: they are not the person's.
const RETIRE_ID_PREFIX: &str = "sot-retire-";

// ----------------------------------------------------------------------------------------------------------------
// The frozen lane.
// ----------------------------------------------------------------------------------------------------------------

const MAGIC: &[u8; 4] = b"SOSV";
const MAX_BODY: usize = 1 << 20;
const PROTO: u32 = 1;

fn put_str(out: &mut Vec<u8>, s: &str) {
    debug_assert!(s.len() <= 128);
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
}

fn framed(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

fn hello_frame(build: &str) -> Vec<u8> {
    let mut body = vec![0x01];
    body.extend_from_slice(&PROTO.to_le_bytes());
    put_str(&mut body, build);
    framed(&body)
}

fn status_frame() -> Vec<u8> {
    framed(&[0x03])
}

fn end_run_frame(operation_id: &str, reason: &str, voyage: &str) -> Vec<u8> {
    let mut body = vec![0x02];
    put_str(&mut body, operation_id);
    body.push(0x01);
    put_str(&mut body, reason);
    put_str(&mut body, voyage);
    framed(&body)
}

fn stop_frame(operation_id: &str) -> Vec<u8> {
    let mut body = vec![0x02];
    put_str(&mut body, operation_id);
    body.push(0x03);
    framed(&body)
}

/// A supervisor's phase, as 0.6 reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Starting,
    Ready,
    Ending,
    EndedNoRespawn,
    Terminal,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Starting => "starting",
            Phase::Ready => "ready",
            Phase::Ending => "ending",
            Phase::EndedNoRespawn => "ended_no_respawn",
            Phase::Terminal => "terminal",
        }
    }
}

/// What an operation reply says (`83`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpState {
    Accepted,
    RecordClosed,
    RecordVerified,
    ResetDone(String),
    Stopping,
    Failed(String),
    Refused(u8),
    Unknown,
}

/// A reply frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reply {
    HelloOk { proto: u32, build: String, pid: u32, created: u64 },
    Refused(u8),
    Operation(OpState),
    StatusOk { pid: u32, created: u64, voyage: Option<String>, leg: Option<u64>, phase: Phase },
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|end| *end <= self.bytes.len()).ok_or("a frame shorter than its fields")?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| "u32")?))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| "u64")?))
    }
    fn string(&mut self) -> Result<String, String> {
        let n = usize::from(self.u8()?);
        if n > 128 {
            return Err("a string over 128 bytes".into());
        }
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "a string that is not UTF-8".into())
    }
    fn flag(&mut self) -> Result<bool, String> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(format!("a flag of {other}")),
        }
    }
    fn done(&self) -> Result<(), String> {
        if self.at == self.bytes.len() { Ok(()) } else { Err("bytes after a frame's last field".into()) }
    }
}

fn decode_reply(body: &[u8]) -> Result<Reply, String> {
    let mut r = Reader { bytes: body, at: 0 };
    let reply = match r.u8()? {
        0x81 => Reply::HelloOk { proto: r.u32()?, build: r.string()?, pid: r.u32()?, created: r.u64()? },
        0x82 => Reply::Refused(r.u8()?),
        0x83 => Reply::Operation(match r.u8()? {
            0x01 => OpState::Accepted,
            0x03 => OpState::RecordClosed,
            0x04 => OpState::RecordVerified,
            0x05 => OpState::ResetDone(r.string()?),
            0x06 => OpState::Stopping,
            0x07 => OpState::Failed(r.string()?),
            0x08 => OpState::Refused(r.u8()?),
            0x09 => OpState::Unknown,
            other => return Err(format!("an operation state of {other}")),
        }),
        0x84 => {
            let pid = r.u32()?;
            let created = r.u64()?;
            let voyage = if r.flag()? { Some(r.string()?) } else { None };
            let leg = if r.flag()? { Some(r.u64()?) } else { None };
            let phase = match r.u8()? {
                0 => Phase::Starting,
                1 => Phase::Ready,
                2 => Phase::Ending,
                3 => Phase::EndedNoRespawn,
                4 => Phase::Terminal,
                other => return Err(format!("a phase of {other}")),
            };
            Reply::StatusOk { pid, created, voyage, leg, phase }
        }
        other => return Err(format!("a reply tag of {other:#x}")),
    };
    r.done()?;
    Ok(reply)
}

/// The first frame in `buf`: its reply and the bytes it used, `None` while it is incomplete.
fn split_reply(buf: &[u8]) -> Result<Option<(Reply, usize)>, String> {
    if buf.len() >= 4 && &buf[..4] != MAGIC {
        return Err("bytes that are not a supervisor frame".into());
    }
    if buf.len() < 8 {
        return Ok(None);
    }
    let len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    if len > MAX_BODY {
        return Err("a frame over 1 MiB".into());
    }
    if buf.len() < 8 + len {
        return Ok(None);
    }
    Ok(Some((decode_reply(&buf[8..8 + len])?, 8 + len)))
}

/// The challenge's own exchange: the frozen hello, and exactly one `HelloOk` as its answer.
struct OldHello {
    buf: Vec<u8>,
}

impl IdentityExchange for OldHello {
    fn encode_request(&self) -> Vec<u8> {
        hello_frame("sotd")
    }
    fn feed(&mut self, bytes: &[u8]) -> ExchangeDecode {
        self.buf.extend_from_slice(bytes);
        match split_reply(&self.buf) {
            Ok(None) => ExchangeDecode::Incomplete,
            Ok(Some((Reply::HelloOk { pid, created, .. }, used))) if used == self.buf.len() => {
                ExchangeDecode::Identity { pid, created }
            }
            _ => ExchangeDecode::Foreign,
        }
    }
}

// ----------------------------------------------------------------------------------------------------------------
// The conversation: the decision logic is written against this, so a scripted fake drives it in unit tests.
// ----------------------------------------------------------------------------------------------------------------

/// What `status` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Status {
    pub phase: Phase,
    pub voyage: Option<String>,
}

/// One old supervisor, proven, as the retire talks to it.
pub(crate) trait Old {
    fn status(&mut self, bound: Duration) -> Result<Status, String>;
    /// `Ok` is an end the supervisor confirmed (its record closed or verified).
    fn end_run(&mut self, voyage: &str, bound: Duration) -> Result<(), String>;
    /// Sends `stop`, waits for its reply within `bound`, and drops the connection (0.6 exits a stopped supervisor once
    /// the connection is gone).
    fn stop(&mut self, bound: Duration) -> Result<(), String>;
    /// Whether the supervisor's process has exited, waiting at most `bound`.
    fn exited(&mut self, bound: Duration) -> Result<bool, String>;
    /// Re-verifies the process and ends it.
    fn terminate(&mut self) -> Result<(), String>;
    /// The supervisor rests after an end: decides, before it is stopped (it is the only witness of a requested end that
    /// no journal holds), whether that end is a person's, and records it durably if so. `Ok(true)` is a person's end.
    fn record_rest(&mut self, voyage: Option<&str>) -> Result<bool, String>;
    fn pause(&mut self, d: Duration);
}

/// What the conversation found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Driven {
    /// The phase the old supervisor reported when settled.
    pub phase: &'static str,
    /// It was resting after an end a person asked for, which is recorded.
    pub live_rest: bool,
    /// The retire's own `end_run` ended a run.
    pub ended_run: bool,
    /// The supervisor had to be terminated.
    pub terminated: bool,
}

/// Ends the old run if there is one, then the old supervisor. `budget` is the shared wait for the run's end.
pub(crate) fn drive(old: &mut dyn Old, budget: Duration, settle_pause: Duration) -> Result<Driven, String> {
    let begun = Instant::now();
    let mut driven = Driven { phase: "unanswered", ..Driven::default() };
    loop {
        let status = match old.status(STATUS_BOUND) {
            Ok(status) => status,
            Err(e) => {
                tracing::warn!(error = %e, "retiring a 0.6 row: its supervisor did not report a status; it is stopped anyway");
                break;
            }
        };
        driven.phase = status.phase.name();
        match status.phase {
            Phase::Starting | Phase::Ending => {
                if begun.elapsed() >= budget {
                    tracing::warn!(phase = driven.phase, "retiring a 0.6 row: its supervisor did not settle within the budget; it is stopped anyway");
                    break;
                }
                old.pause(settle_pause);
            }
            Phase::Ready => {
                match status.voyage {
                    Some(voyage) => match old.end_run(&voyage, budget.saturating_sub(begun.elapsed())) {
                        Ok(()) => driven.ended_run = true,
                        Err(e) => tracing::warn!(error = %e, "retiring a 0.6 row: its run was not confirmed ended; its supervisor is stopped anyway"),
                    },
                    None => tracing::warn!("retiring a 0.6 row: a ready supervisor named no voyage"),
                }
                break;
            }
            Phase::EndedNoRespawn => {
                driven.live_rest = old.record_rest(status.voyage.as_deref())?;
                break;
            }
            Phase::Terminal => break,
        }
    }
    if let Err(e) = old.stop(STOP_REPLY_BOUND) {
        tracing::warn!(error = %e, "retiring a 0.6 row: the stop was not acknowledged");
    }
    if old.exited(EXIT_BOUND)? {
        return Ok(driven);
    }
    old.terminate()?;
    driven.terminated = true;
    if old.exited(KILL_WAIT_BOUND)? {
        Ok(driven)
    } else {
        Err("the 0.6 supervisor did not exit".to_string())
    }
}

type Conn = <PlatformEndpoint as Endpoint>::Client;
type Process = <PlatformEndpoint as Endpoint>::Process;

/// The 0.6 supervisor's endpoint for `state_dir`.
fn endpoint(state_dir: &Path) -> Result<PathBuf, String> {
    let h = sot_platform::host::state_dir::state_dir_hash(state_dir);
    #[cfg(unix)]
    {
        Ok(sot_platform::host::state_dir::runtime_dir().map_err(|e| e.to_string())?.join(format!("supervisor-{h}.sock")))
    }
    #[cfg(windows)]
    {
        Ok(PathBuf::from(format!(r"\\.\pipe\sot-supervisor-{h}")))
    }
}

/// How a dial found the old supervisor.
enum Found {
    /// Nobody answers, or what answers is not proven to be a 0.6 supervisor: nothing is sent.
    Nobody,
    Proven(Conn, Process),
}

fn dial(state_dir: &Path) -> Found {
    let path = match endpoint(state_dir) {
        Ok(path) => path,
        Err(e) => {
            tracing::debug!(error = %e, "retiring a 0.6 row: no endpoint to dial");
            return Found::Nobody;
        }
    };
    let conn = match connect_own(&path) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::debug!(error = %e, "retiring a 0.6 row: no supervisor answers");
            return Found::Nobody;
        }
    };
    let mut hello = OldHello { buf: Vec::new() };
    match PlatformEndpoint::default().challenge(&conn, &mut hello, Instant::now() + CHALLENGE_BOUND) {
        ChallengeOutcome::Proven(process) => Found::Proven(conn, process),
        ChallengeOutcome::Foreign => {
            tracing::warn!(endpoint = ?path, "retiring a 0.6 row: what answers on the old supervisor's endpoint is not a 0.6 supervisor; nothing was sent");
            Found::Nobody
        }
        ChallengeOutcome::Undetermined => {
            tracing::warn!(endpoint = ?path, "retiring a 0.6 row: the old supervisor's endpoint could not be proven; nothing was sent");
            Found::Nobody
        }
    }
}

/// The real conversation over a proven connection.
struct Real {
    state_dir: PathBuf,
    conn: Option<Conn>,
    process: Process,
    pending: Vec<u8>,
}

impl Real {
    /// The connection, dialed and proven again when a cancelled step closed it; it must reach the same process.
    fn conn(&mut self) -> Result<&Conn, String> {
        if self.conn.is_none() {
            self.pending.clear();
            match dial(&self.state_dir) {
                Found::Proven(conn, process) if process.pid() == self.process.pid() && process.created() == self.process.created() => {
                    self.conn = Some(conn);
                }
                Found::Proven(..) => return Err("the old supervisor's endpoint now answers as another process".into()),
                Found::Nobody => return Err("the old supervisor no longer answers".into()),
            }
        }
        Ok(self.conn.as_ref().expect("just set"))
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), String> {
        let sent = self.conn()?.write_all(frame).map_err(|e| e.to_string());
        if sent.is_err() {
            self.conn = None;
        }
        sent
    }

    /// The next reply within `bound`. A timeout cancels the connection, which is then dropped.
    fn reply(&mut self, bound: Duration) -> Result<Reply, String> {
        self.conn()?;
        let conn = self.conn.as_ref().expect("dialed");
        let pending = &mut self.pending;
        let read = run_with_deadline(
            Instant::now() + bound,
            || conn.cancel(),
            || -> Result<Reply, String> {
                loop {
                    if let Some((reply, used)) = split_reply(pending)? {
                        pending.drain(..used);
                        return Ok(reply);
                    }
                    let mut buf = [0u8; 512];
                    let n = conn.read(&mut buf).map_err(|e| e.to_string())?;
                    if n == 0 {
                        return Err("the connection closed".into());
                    }
                    pending.extend_from_slice(&buf[..n]);
                }
            },
        );
        let read = read.unwrap_or_else(|| Err(format!("no reply within {bound:?}")));
        if read.is_err() {
            self.conn = None;
        }
        read
    }
}

impl Old for Real {
    fn status(&mut self, bound: Duration) -> Result<Status, String> {
        self.send(&status_frame())?;
        match self.reply(bound)? {
            Reply::StatusOk { phase, voyage, .. } => Ok(Status { phase, voyage }),
            other => Err(format!("a reply that is not a status: {other:?}")),
        }
    }

    fn end_run(&mut self, voyage: &str, bound: Duration) -> Result<(), String> {
        let id = format!("{RETIRE_ID_PREFIX}end-{}", uuid::Uuid::now_v7().simple());
        self.send(&end_run_frame(&id, "retired by 0.7", voyage))?;
        let deadline = Instant::now() + bound;
        loop {
            match self.reply(deadline.saturating_duration_since(Instant::now()))? {
                Reply::Operation(OpState::Accepted) => {}
                Reply::Operation(OpState::RecordClosed | OpState::RecordVerified) => return Ok(()),
                other => return Err(format!("the end was not confirmed: {other:?}")),
            }
        }
    }

    fn stop(&mut self, bound: Duration) -> Result<(), String> {
        let id = format!("{RETIRE_ID_PREFIX}stop-{}", uuid::Uuid::now_v7().simple());
        let answered = self.send(&stop_frame(&id)).and_then(|()| {
            let deadline = Instant::now() + bound;
            loop {
                match self.reply(deadline.saturating_duration_since(Instant::now()))? {
                    Reply::Operation(OpState::Accepted) => {}
                    Reply::Operation(OpState::Stopping) => return Ok(()),
                    other => return Err(format!("a reply that is not a stop's: {other:?}")),
                }
            }
        });
        // 0.6 exits a stopped supervisor once its connection is gone.
        self.conn = None;
        answered
    }

    fn exited(&mut self, bound: Duration) -> Result<bool, String> {
        self.process.wait(bound).map_err(|e| format!("waiting for the 0.6 supervisor: {e}"))
    }

    fn terminate(&mut self) -> Result<(), String> {
        match self.process.reverify() {
            Ok(true) => self.process.terminate().map_err(|e| format!("ending the 0.6 supervisor: {e}")),
            // A process that is gone, or is no longer the one proven, is not ours to end.
            Ok(false) => Ok(()),
            Err(e) => Err(format!("re-verifying the 0.6 supervisor: {e}")),
        }
    }

    fn pause(&mut self, d: Duration) {
        std::thread::sleep(d);
    }

    fn record_rest(&mut self, voyage: Option<&str>) -> Result<bool, String> {
        record_live_rest(&self.state_dir, voyage)
    }
}

/// A resting 0.6 supervisor's end is the retire's own when the journal holds a closed or verified `end_run` on that voyage
/// with a retire's operation id and none that is not; otherwise it is a person's, and `ended` is written before the
/// supervisor is stopped. `Ok(true)` is a person's end.
fn record_live_rest(state_dir: &Path, voyage: Option<&str>) -> Result<bool, String> {
    let voyage = match voyage {
        Some(v) => Some(v.to_string()),
        None => pointer_voyage(state_dir).unwrap_or(None),
    };
    let own = voyage.is_some_and(|v| match journal_ends(state_dir, &v) {
        Ok(ends) => ends.retire && !ends.person,
        Err(e) => {
            tracing::warn!(error = %e, "retiring a 0.6 row: the old journal could not be read; the rest is kept as a person's");
            false
        }
    });
    if own {
        return Ok(false);
    }
    sot_capsule::capsule::mark_end_requested(state_dir, "ended under 0.6").map_err(|e| format!("record the requested end: {e}"))?;
    Ok(true)
}

// ----------------------------------------------------------------------------------------------------------------
// The folder: its layout, a requested end read from the 0.6 journal, and the move.
// ----------------------------------------------------------------------------------------------------------------

/// The names in `state_dir` a 0.6 release left, present as anything (a link is not followed).
pub(crate) fn layout(state_dir: &Path) -> Result<Vec<OsString>, String> {
    let entries = match std::fs::read_dir(state_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {state_dir:?}: {e}")),
    };
    let mut found = Vec::new();
    for entry in entries {
        let name = entry.map_err(|e| format!("read {state_dir:?}: {e}"))?.file_name();
        let text = name.to_string_lossy();
        if LAYOUT_NAMES.contains(&text.as_ref()) || text.starts_with("drawer.voyage.") {
            found.push(name);
        }
    }
    found.sort();
    Ok(found)
}

fn is_regular_file(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => Ok(m.file_type().is_file()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{path:?}: {e}")),
    }
}

/// A JSON object read from a small regular file whose `schema_version` is 1.
fn read_record(path: &Path) -> Result<serde_json::Value, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{path:?}: {e}"))?;
    if !meta.file_type().is_file() || meta.len() > 16 * 1024 {
        return Err(format!("{path:?}: not a regular file of at most 16 KiB"));
    }
    let text = std::fs::read(path).map_err(|e| format!("{path:?}: {e}"))?;
    let value: serde_json::Value = serde_json::from_slice(&text).map_err(|e| format!("{path:?}: {e}"))?;
    if value.as_object().is_some_and(|o| o.get("schema_version").and_then(serde_json::Value::as_u64) == Some(1)) {
        Ok(value)
    } else {
        Err(format!("{path:?}: not an object of schema 1"))
    }
}

/// A completion entry of the 0.6 journal (`.closed` or `.terminal`) beside an `.active` record: `None` when absent, an
/// `Err` when present and not a regular file of schema 1.
fn completion(path: &Path) -> Result<Option<serde_json::Value>, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{path:?}: {e}")),
        Ok(_) => read_record(path).map(Some),
    }
}

/// Who asked for the ends the 0.6 journal records on one voyage.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Ends {
    /// A closed or verified `end_run` that is not this retire's.
    pub person: bool,
    /// A closed or verified `end_run` whose operation id starts [`RETIRE_ID_PREFIX`].
    pub retire: bool,
}

/// The closed or verified `end_run`s the journal holds on `voyage`, told apart by their operation ids. Anything present
/// that cannot be read is an `Err` naming the file.
pub(crate) fn journal_ends(state_dir: &Path, voyage: &str) -> Result<Ends, String> {
    let dir = state_dir.join("supervisor-journal");
    let entries = match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ends::default()),
        Err(e) => return Err(format!("{dir:?}: {e}")),
        Ok(m) if !m.file_type().is_dir() => return Err(format!("{dir:?}: not a directory")),
        Ok(_) => std::fs::read_dir(&dir).map_err(|e| format!("{dir:?}: {e}"))?,
    };
    let mut ends = Ends::default();
    for entry in entries {
        let name = entry.map_err(|e| format!("{dir:?}: {e}"))?.file_name();
        let name = name.to_string_lossy();
        let Some(stem) = name.strip_suffix(".active") else { continue };
        if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            continue;
        }
        let record = read_record(&dir.join(name.as_ref()))?;
        let op = record.get("op").and_then(serde_json::Value::as_object);
        let Some(kind) = op.and_then(|o| o.get("op")).and_then(serde_json::Value::as_str) else {
            return Err(format!("{:?}: no operation", dir.join(name.as_ref())));
        };
        let id = record.get("operation_id").and_then(serde_json::Value::as_str).unwrap_or_default();
        let on_voyage = op.and_then(|o| o.get("voyage")).and_then(serde_json::Value::as_str) == Some(voyage);
        if kind != "end_run" || !on_voyage {
            continue;
        }
        let ended = completion(&dir.join(format!("{stem}.closed")))?.is_some()
            || completion(&dir.join(format!("{stem}.terminal")))?
                .is_some_and(|t| t.get("state").and_then(serde_json::Value::as_str) == Some("record_verified"));
        if ended {
            if id.starts_with(RETIRE_ID_PREFIX) {
                ends.retire = true;
            } else {
                ends.person = true;
            }
        }
    }
    Ok(ends)
}

/// The voyage the 0.6 pointer names: `None` when there is no pointer, an `Err` when it is not a canonical lowercase UUID.
fn pointer_voyage(state_dir: &Path) -> Result<Option<String>, String> {
    let pointer = state_dir.join("drawer.voyage");
    match std::fs::symlink_metadata(&pointer) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{pointer:?}: {e}")),
        Ok(m) if !m.file_type().is_file() => Err(format!("{pointer:?}: not a regular file")),
        Ok(_) => {
            let bytes = std::fs::read(&pointer).map_err(|e| format!("{pointer:?}: {e}"))?;
            let text = String::from_utf8(bytes).map_err(|_| format!("{pointer:?}: not UTF-8"))?;
            let parsed = uuid::Uuid::parse_str(&text).map_err(|e| format!("{pointer:?}: {e}"))?;
            if text.len() != 36 || text != parsed.hyphenated().to_string() {
                return Err(format!("{pointer:?}: not a canonical lowercase UUID"));
            }
            Ok(Some(text))
        }
    }
}

/// Whether a person asked for the end of the row's last 0.6 run, read from the 0.6 journal when no supervisor lives to say.
/// `Ok(false)` is no evidence of one. Anything present that cannot be read is an `Err` naming the file: the caller rests
/// the row, since a wrong rest is undone by one selection and a wrong start cannot be undone.
pub(crate) fn journal_says_requested(state_dir: &Path) -> Result<bool, String> {
    match pointer_voyage(state_dir)? {
        None => Ok(false),
        Some(voyage) => Ok(journal_ends(state_dir, &voyage)?.person),
    }
}

/// Takes and drops each 0.6 terminal's writer lock: a held one is a terminal still running. Each is dropped at once, since
/// Windows cannot move a folder with an open file in it.
fn prove_no_terminal(state_dir: &Path) -> Result<(), String> {
    let voyages = state_dir.join("voyages");
    let entries = match std::fs::symlink_metadata(&voyages) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{voyages:?}: {e}")),
        Ok(m) if !m.file_type().is_dir() => return Ok(()),
        Ok(_) => std::fs::read_dir(&voyages).map_err(|e| format!("{voyages:?}: {e}"))?,
    };
    for entry in entries {
        let path = entry.map_err(|e| format!("{voyages:?}: {e}"))?.path();
        let is_dir = std::fs::symlink_metadata(&path).map(|m| m.file_type().is_dir()).unwrap_or(false);
        let lock = path.join("writer.lock");
        if !is_dir || !is_regular_file(&lock)? {
            continue;
        }
        if let Err(e) = sot_platform::host::lock_writer(&lock) {
            return Err(format!(
                "a 0.6 terminal still runs in this row: {lock:?} is held ({e}); end that sot-capsule run process, then select the row"
            ));
        }
    }
    Ok(())
}

/// What the folder step did.
#[derive(Debug, Default)]
struct Moved {
    requested: bool,
    names: Vec<String>,
}

/// Under the fence: no 0.6 terminal runs; a requested end the journal holds is made durable before anything moves; then the layout moves
/// into [`RETIRED_DIR`].
fn move_layout(state_dir: &Path, found: &[OsString]) -> Result<Moved, String> {
    prove_no_terminal(state_dir)?;
    let requested = journal_says_requested(state_dir).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "retiring a 0.6 row: the old journal could not be read; the row rests until selected");
        true
    });
    if requested {
        sot_capsule::capsule::mark_end_requested(state_dir, "ended under 0.6").map_err(|e| format!("record the requested end: {e}"))?;
    }
    let dest = state_dir.join(RETIRED_DIR);
    match std::fs::symlink_metadata(&dest) {
        Ok(m) if m.file_type().is_dir() => {}
        Ok(_) => return Err(format!("{dest:?} exists and is not a directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&dest).map_err(|e| format!("create {dest:?}: {e}"))?,
        Err(e) => return Err(format!("{dest:?}: {e}")),
    }
    let mut names = Vec::new();
    for name in found {
        let to = dest.join(name);
        if std::fs::symlink_metadata(&to).is_ok() {
            return Err(format!("{to:?} already exists"));
        }
        std::fs::rename(state_dir.join(name), &to).map_err(|e| format!("move {name:?} into {dest:?}: {e}"))?;
        names.push(name.to_string_lossy().into_owned());
    }
    Ok(Moved { requested, names })
}

/// Retires the 0.6 row in `state_dir`, if it is one; a 0.7 folder costs one directory read. The caller holds the row's
/// guard. `Ok` means no 0.6 process holds the row and its files are moved aside.
pub(crate) fn retire_06(state_dir: &Path) -> Result<(), String> {
    let found = layout(state_dir)?;
    if found.is_empty() {
        return Ok(());
    }
    let (mut phase, mut ended_run) = ("no live supervisor", false);
    // The row's own scopes (Linux): the supervisor's, captured before it is stopped, and every one the durable `row-scopes`
    // record remembers, which a retry after a daemon's death has no live supervisor to find again.
    #[cfg(target_os = "linux")]
    let (root, own) = (crate::rows::spawn::row_scope::root(), crate::rows::spawn::row_scope::own_rel().unwrap_or_default());
    #[cfg(target_os = "linux")]
    let mut captured = None;
    if let Found::Proven(conn, process) = dial(state_dir) {
        #[cfg(target_os = "linux")]
        {
            captured = crate::rows::spawn::row_scope::capture(&root, state_dir, process.pid())?;
        }
        let mut old = Real { state_dir: state_dir.to_path_buf(), conn: Some(conn), process, pending: Vec::new() };
        let driven = drive(&mut old, QUIT_CUTOFF, SETTLE_PAUSE)?;
        (phase, ended_run) = (driven.phase, driven.ended_run);
    }
    #[cfg(target_os = "linux")]
    crate::rows::spawn::row_scope::end(&root, &own, state_dir, captured.as_deref(), crate::rows::spawn::row_scope::SCOPE_EMPTY_BOUND)?;
    let moved = if sot_capsule::supervisor::fence::supervisor_lock_path(state_dir).is_file() {
        match absence_proof_then(state_dir, row_socket_silent, || move_layout(state_dir, &found)) {
            Ok(Some(moved)) => moved?,
            Ok(None) => return Err("a row socket answers and no 0.6 supervisor ended it".to_string()),
            Err(NotProven::FenceUnavailable) => return Err("a process holds this row's fence and no 0.6 supervisor ended it".to_string()),
            Err(NotProven::RowSocketUnjudged(detail)) => return Err(detail),
        }
    } else {
        // No fence file: a start was never made here, and nothing can hold an absent fence. The creating fence is not
        // taken, since the file it would make is the start marker a boot resume reads.
        match row_socket_silent(state_dir) {
            Ok(true) => move_layout(state_dir, &found)?,
            Ok(false) => return Err("a row socket answers and no 0.6 supervisor ended it".to_string()),
            Err(detail) => return Err(detail),
        }
    };
    tracing::info!(
        folder = ?state_dir, old_phase = phase, ended_run, requested_end = moved.requested, moved = ?moved.names,
        "retired a 0.6 row"
    );
    Ok(())
}

#[cfg(test)]
#[path = "retire_06_tests.rs"]
mod tests;
