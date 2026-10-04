//! The SOSV (supervisor) lane: operation-id validation, frame types, encoders and body decoder.

use super::*;


/// The supervisor lane's `build`/`reason`/`detail`/voyage-id string
/// fields all share this one bound (like `MAX_CONTROLLER_ID_LEN`,
/// generous enough for a UUID or a short diagnostic, never a data
/// payload). `operation_id` does NOT use this bound — see
/// [`MAX_OPERATION_ID_LEN`] and [`validate_operation_id`].
pub const MAX_SUPERVISOR_STRING_LEN: usize = 128;
const _: () = assert!(MAX_SUPERVISOR_STRING_LEN <= u8::MAX as usize);

/// `operation_id`'s own, TIGHTER bound (ADR 0041 step 6 U2, Codex review
/// finding 6): it is interpolated directly into a filesystem path
/// (`<state_dir>/supervisor-journal/<id>.*`), never merely displayed, so
/// its charset is restricted to `[A-Za-z0-9._-]` (see
/// [`validate_operation_id`]) and its length to a conservative 64 bytes
/// — ample for a UUID or a short caller-chosen token, far short of any
/// filesystem component limit.
pub const MAX_OPERATION_ID_LEN: usize = 64;
const _: () = assert!(MAX_OPERATION_ID_LEN <= u8::MAX as usize);

/// `true` iff every byte of `s` is `[A-Za-z0-9._-]` — the only characters
/// [`validate_operation_id`] ever allows through.
fn is_operation_id_charset(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// `operation_id` is used as a Windows filesystem path component with no
/// further sanitization downstream (`journal.rs` interpolates it
/// directly into `<id>.active`/`<id>.terminal`/`<id>.closed`) — this is
/// the ONE place that must refuse everything a path component must never
/// be: separators (rejected by the charset itself, which excludes `/`
/// and `\`), an absolute-path prefix (likewise, no `:` or leading
/// separator in the allowed set), and the two reserved relative-path
/// names `.`/`..`, which the charset alone cannot exclude since both are
/// built entirely from otherwise-legal characters. Called at WIRE DECODE
/// time (`decode_supervisor_body`'s `command`/`query` arms) so the
/// journal itself never sees an unvalidated id — length is already
/// bounded by [`MAX_OPERATION_ID_LEN`] before this runs.
fn validate_operation_id(field: &'static str, s: String) -> Result<String, WireError> {
    if s == "." || s == ".." || !is_operation_id_charset(&s) {
        return Err(WireError::InvalidOperationId { field, value: s });
    }
    Ok(s)
}

/// The only supervisor-lane protocol version this build speaks — there is
/// no negotiation (unlike the attach lane's `hello`): a mismatch is a
/// hard `Refused { reason: VersionSkew }`, closing the connection (ADR
/// 0041: "the lane rejects the pair it did not [recognize]").
pub const SUPERVISOR_PROTO_V1: u32 = 1;

/// The supervisor lane's one closed refusal-reason enum, shared by
/// `hello`'s top-level `Refused` (only ever `VersionSkew`) and a
/// `command`/`query`'s own `Operation(Refused { .. })` (only ever
/// `StaleVoyage`/`IdConflict` — ADR 0041: "a mismatch is `refused
/// {stale_voyage}`"; "resubmitted with a DIFFERENT command digest is
/// `refused {id_conflict}`"). One enum, not three, since nothing about
/// decoding a reason byte depends on which context it arrived in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SupervisorRefusedReason {
    VersionSkew = 0,
    StaleVoyage = 1,
    IdConflict = 2,
}

impl TryFrom<u8> for SupervisorRefusedReason {
    type Error = WireError;
    fn try_from(value: u8) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::VersionSkew),
            1 => Ok(Self::StaleVoyage),
            2 => Ok(Self::IdConflict),
            other => Err(WireError::UnknownEnumValue {
                field: "supervisor.refused.reason",
                value: other,
            }),
        }
    }
}

/// `status_ok`'s `phase` field (ADR 0041 Lifecycle: "`phase` is total over
/// the authority's own life").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SupervisorPhase {
    Starting = 0,
    Ready = 1,
    Ending = 2,
    EndedNoRespawn = 3,
    Terminal = 4,
}

impl TryFrom<u8> for SupervisorPhase {
    type Error = WireError;
    fn try_from(value: u8) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::Starting),
            1 => Ok(Self::Ready),
            2 => Ok(Self::Ending),
            3 => Ok(Self::EndedNoRespawn),
            4 => Ok(Self::Terminal),
            other => Err(WireError::UnknownEnumValue {
                field: "supervisor.status_ok.phase",
                value: other,
            }),
        }
    }
}

/// The supervisor lane, client→server (ADR 0041 Lifecycle "The lane's
/// operations are one command family, one query family, and one
/// stateless request"). `Hello` MUST be the first frame of every
/// connection (see its own doc); every request on this lane composes
/// with the same-connection challenge — see [`crate::challenge`] — which
/// this module has no opinion on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorRequest {
    /// The FIRST frame of every connection. `proto` is this lane's own
    /// version (see [`SUPERVISOR_PROTO_V1`]); `build` is a caller-supplied
    /// build-compatibility string — "a build identity is compatibility
    /// data, not a credential" (ADR 0041). Doubles as the same-connection
    /// challenge's own steps 4-5 identity-yielding exchange: the reply
    /// carries this process's pid/creation time for the caller to bind
    /// against (see [`SupervisorReply::HelloOk`]).
    Hello { proto: u32, build: String },
    /// `op` ∈ `{end_run, reset, stop}`. `operation_id` is durable for
    /// every mutating op (ADR 0041: "`operation_id` is durable for
    /// MUTATING ops only").
    Command {
        operation_id: String,
        op: SupervisorOp,
    },
    /// A SEPARATE STATELESS REQUEST — no `operation_id`, mutates nothing.
    Status,
    /// Poll a previously submitted `operation_id`'s current state.
    Query { operation_id: String },
}

/// `command`'s `op` (ADR 0041 Lifecycle: "`op` ∈ { `end_run {reason,
/// voyage}`, `reset {voyage?}`, `stop` }").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorOp {
    /// `voyage` is the voyage the caller OBSERVED — lifecycle commands
    /// are VOYAGE-FENCED (ADR 0041: a mismatch is
    /// `refused {stale_voyage}` with no mutation).
    EndRun { reason: String, voyage: String },
    /// `voyage`, if supplied, is likewise the caller's observed voyage,
    /// fencing a reset aimed at a live pointer the same way `end_run`
    /// does; `None` resets an absent/corrupt pointer, which has no live
    /// voyage to fence against.
    Reset { voyage: Option<String> },
    Stop,
}

/// The supervisor lane, server→client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorReply {
    /// `proto`/`build` echo this server's own values (so a caller can log
    /// exactly what disagreed, on the rare path where a build check
    /// upstream of this lane let a near-miss through); `pid`/`created` are
    /// this process's own identity, read directly off the OS (never
    /// trusted before the same-connection challenge's SID step already
    /// succeeded — see [`crate::challenge`]).
    HelloOk {
        proto: u32,
        build: String,
        pid: u32,
        created: u64,
    },
    /// `hello`'s own refusal — always `VersionSkew` in practice; the
    /// connection closes after this (ADR 0041: "a mismatch is answered
    /// `refused {version_skew}` and closed").
    Refused { reason: SupervisorRefusedReason },
    /// `command`'s and `query`'s shared reply shape — see
    /// [`SupervisorOperationState`]'s own doc for why these are ONE
    /// vocabulary, not two.
    Operation(SupervisorOperationState),
    /// `phase` is total over the authority's own life; `leg` is optional
    /// and voyage-qualified (ADR 0041: "the mandatory first `status` of
    /// every client must be able to say 'no leg yet'").
    StatusOk {
        pid: u32,
        created: u64,
        voyage: Option<String>,
        leg: Option<u64>,
        phase: SupervisorPhase,
    },
}

/// What a `command` or a `query` answers with — ADR 0041 Lifecycle:
/// "`query {operation_id}` returns `accepted` | `in_progress` |
/// `record_closed` | `record_verified` | `reset_done {new_voyage}` |
/// `stopping` | `failed {detail}` | `refused {reason}` |
/// `unknown_operation`." A `command`'s own immediate reply uses this
/// SAME vocabulary (typically `Accepted`, once the operation is
/// durably journaled) rather than blocking for a later state — "the FE
/// never blocks on an O(history) walk" — so `command` and `query` share
/// one wire shape instead of two that could drift.
///
/// `in_progress` is DELETED from this implementation (Codex review round
/// 1, simplicity audit): this crate never emits a distinction between
/// "accepted, not yet acted on" and "accepted, currently being acted
/// on" finer than `Accepted` itself — inventing a second wire value this
/// authority can never actually produce would only be a state nothing
/// tests or exercises. The ADR's own vocabulary lists `in_progress` as
/// available to a future, richer authority; this one has no caller of
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorOperationState {
    Accepted,
    RecordClosed,
    RecordVerified,
    ResetDone { new_voyage: String },
    Stopping,
    Failed { detail: String },
    Refused { reason: SupervisorRefusedReason },
    /// Returned for a MISSING journal entry and ONLY that — "the one
    /// state meaning SAFE TO RESUBMIT" (ADR 0041).
    UnknownOperation,
}

fn push_supervisor_op(body: &mut Vec<u8>, op: &SupervisorOp) -> Result<(), WireError> {
    match op {
        SupervisorOp::EndRun { reason, voyage } => {
            body.push(TAG_SV_OP_END_RUN);
            push_bounded_string(body, reason, MAX_SUPERVISOR_STRING_LEN, "command.end_run.reason", false)?;
            push_bounded_string(body, voyage, MAX_SUPERVISOR_STRING_LEN, "command.end_run.voyage", true)?;
        }
        SupervisorOp::Reset { voyage } => {
            body.push(TAG_SV_OP_RESET);
            body.push(voyage.is_some() as u8);
            if let Some(voyage) = voyage {
                push_bounded_string(body, voyage, MAX_SUPERVISOR_STRING_LEN, "command.reset.voyage", true)?;
            }
        }
        SupervisorOp::Stop => body.push(TAG_SV_OP_STOP),
    }
    Ok(())
}

/// The canonical, stable byte encoding of a supervisor-lane `op` — the
/// SAME tag/length-prefixed encoding `command`'s own wire body already
/// carries it in (`push_supervisor_op`), reused here rather than a
/// second, divergent encoding of the same data. This is what the
/// journal's own `id_conflict` digest is computed over (ADR 0041 step 6
/// U2, Codex review finding 6: `format!("{op:?}")` is a `Debug` string,
/// "neither a digest nor a stable durable encoding across builds" —
/// `Debug`'s own output format is not part of any stability contract).
/// Stable across builds because it is exactly the bytes this crate must
/// already keep stable for the wire protocol itself; the caller hashes
/// this (SHA-256 today) rather than storing or comparing it raw.
pub fn canonical_supervisor_op_bytes(op: &SupervisorOp) -> Result<Vec<u8>, WireError> {
    let mut body = Vec::new();
    push_supervisor_op(&mut body, op)?;
    Ok(body)
}

fn push_supervisor_operation_state(
    body: &mut Vec<u8>,
    state: &SupervisorOperationState,
) -> Result<(), WireError> {
    match state {
        SupervisorOperationState::Accepted => body.push(TAG_SV_OPSTATE_ACCEPTED),
        SupervisorOperationState::RecordClosed => body.push(TAG_SV_OPSTATE_RECORD_CLOSED),
        SupervisorOperationState::RecordVerified => body.push(TAG_SV_OPSTATE_RECORD_VERIFIED),
        SupervisorOperationState::ResetDone { new_voyage } => {
            body.push(TAG_SV_OPSTATE_RESET_DONE);
            push_bounded_string(body, new_voyage, MAX_SUPERVISOR_STRING_LEN, "operation.reset_done.new_voyage", true)?;
        }
        SupervisorOperationState::Stopping => body.push(TAG_SV_OPSTATE_STOPPING),
        SupervisorOperationState::Failed { detail } => {
            body.push(TAG_SV_OPSTATE_FAILED);
            push_bounded_string(body, detail, MAX_SUPERVISOR_STRING_LEN, "operation.failed.detail", false)?;
        }
        SupervisorOperationState::Refused { reason } => {
            body.push(TAG_SV_OPSTATE_REFUSED);
            body.push(*reason as u8);
        }
        SupervisorOperationState::UnknownOperation => body.push(TAG_SV_OPSTATE_UNKNOWN_OPERATION),
    }
    Ok(())
}

/// Encodes a supervisor-lane client→server frame as a complete `SOSV`
/// wire frame.
pub fn encode_supervisor_request(frame: &SupervisorRequest) -> Result<Vec<u8>, WireError> {
    let mut body = Vec::new();
    match frame {
        SupervisorRequest::Hello { proto, build } => {
            body.push(TAG_SV_REQ_HELLO);
            push_u32(&mut body, *proto);
            push_bounded_string(&mut body, build, MAX_SUPERVISOR_STRING_LEN, "hello.build", false)?;
        }
        SupervisorRequest::Command { operation_id, op } => {
            body.push(TAG_SV_REQ_COMMAND);
            push_bounded_string(&mut body, operation_id, MAX_OPERATION_ID_LEN, "command.operation_id", true)?;
            push_supervisor_op(&mut body, op)?;
        }
        SupervisorRequest::Status => body.push(TAG_SV_REQ_STATUS),
        SupervisorRequest::Query { operation_id } => {
            body.push(TAG_SV_REQ_QUERY);
            push_bounded_string(&mut body, operation_id, MAX_OPERATION_ID_LEN, "query.operation_id", true)?;
        }
    }
    wrap(SUPERVISOR_MAGIC, body)
}

/// Encodes a supervisor-lane server→client frame as a complete `SOSV`
/// wire frame.
pub fn encode_supervisor_reply(frame: &SupervisorReply) -> Result<Vec<u8>, WireError> {
    let mut body = Vec::new();
    match frame {
        SupervisorReply::HelloOk { proto, build, pid, created } => {
            body.push(TAG_SV_REP_HELLO_OK);
            push_u32(&mut body, *proto);
            push_bounded_string(&mut body, build, MAX_SUPERVISOR_STRING_LEN, "hello_ok.build", false)?;
            push_u32(&mut body, *pid);
            push_u64(&mut body, *created);
        }
        SupervisorReply::Refused { reason } => {
            body.push(TAG_SV_REP_REFUSED);
            body.push(*reason as u8);
        }
        SupervisorReply::Operation(state) => {
            body.push(TAG_SV_REP_OPERATION);
            push_supervisor_operation_state(&mut body, state)?;
        }
        SupervisorReply::StatusOk { pid, created, voyage, leg, phase } => {
            body.push(TAG_SV_REP_STATUS_OK);
            push_u32(&mut body, *pid);
            push_u64(&mut body, *created);
            body.push(voyage.is_some() as u8);
            if let Some(voyage) = voyage {
                push_bounded_string(&mut body, voyage, MAX_SUPERVISOR_STRING_LEN, "status_ok.voyage", true)?;
            }
            body.push(leg.is_some() as u8);
            if let Some(leg) = leg {
                push_u64(&mut body, *leg);
            }
            body.push(*phase as u8);
        }
    }
    wrap(SUPERVISOR_MAGIC, body)
}

fn read_supervisor_op(r: &mut Reader) -> Result<SupervisorOp, WireError> {
    let tag = r.u8("command.op.tag")?;
    Ok(match tag {
        TAG_SV_OP_END_RUN => {
            let reason = r.bounded_string(MAX_SUPERVISOR_STRING_LEN, "command.end_run.reason", false)?;
            let voyage = r.bounded_string(MAX_SUPERVISOR_STRING_LEN, "command.end_run.voyage", true)?;
            SupervisorOp::EndRun { reason, voyage }
        }
        TAG_SV_OP_RESET => {
            let has_voyage = r.bool_flag("command.reset.has_voyage")?;
            let voyage = if has_voyage {
                Some(r.bounded_string(MAX_SUPERVISOR_STRING_LEN, "command.reset.voyage", true)?)
            } else {
                None
            };
            SupervisorOp::Reset { voyage }
        }
        TAG_SV_OP_STOP => SupervisorOp::Stop,
        other => return Err(WireError::UnknownTag(other)),
    })
}

fn read_supervisor_operation_state(r: &mut Reader) -> Result<SupervisorOperationState, WireError> {
    let tag = r.u8("operation.tag")?;
    Ok(match tag {
        TAG_SV_OPSTATE_ACCEPTED => SupervisorOperationState::Accepted,
        TAG_SV_OPSTATE_RECORD_CLOSED => SupervisorOperationState::RecordClosed,
        TAG_SV_OPSTATE_RECORD_VERIFIED => SupervisorOperationState::RecordVerified,
        TAG_SV_OPSTATE_RESET_DONE => {
            let new_voyage = r.bounded_string(MAX_SUPERVISOR_STRING_LEN, "operation.reset_done.new_voyage", true)?;
            SupervisorOperationState::ResetDone { new_voyage }
        }
        TAG_SV_OPSTATE_STOPPING => SupervisorOperationState::Stopping,
        TAG_SV_OPSTATE_FAILED => {
            let detail = r.bounded_string(MAX_SUPERVISOR_STRING_LEN, "operation.failed.detail", false)?;
            SupervisorOperationState::Failed { detail }
        }
        TAG_SV_OPSTATE_REFUSED => {
            let reason = SupervisorRefusedReason::try_from(r.u8("operation.refused.reason")?)?;
            SupervisorOperationState::Refused { reason }
        }
        TAG_SV_OPSTATE_UNKNOWN_OPERATION => SupervisorOperationState::UnknownOperation,
        other => return Err(WireError::UnknownTag(other)),
    })
}

pub(super) fn decode_supervisor_body(body: &[u8]) -> Result<DecodedFrame, WireError> {
    let mut r = Reader::new(body);
    let tag = r.u8("tag")?;
    let frame = match tag {
        TAG_SV_REQ_HELLO => {
            let proto = r.u32("hello.proto")?;
            let build = r.bounded_string(MAX_SUPERVISOR_STRING_LEN, "hello.build", false)?;
            r.finish("hello")?;
            DecodedFrame::SupervisorRequest(SupervisorRequest::Hello { proto, build })
        }
        TAG_SV_REQ_COMMAND => {
            let operation_id = r.bounded_string(MAX_OPERATION_ID_LEN, "command.operation_id", true)?;
            let operation_id = validate_operation_id("command.operation_id", operation_id)?;
            let op = read_supervisor_op(&mut r)?;
            r.finish("command")?;
            DecodedFrame::SupervisorRequest(SupervisorRequest::Command { operation_id, op })
        }
        TAG_SV_REQ_STATUS => {
            r.finish("status")?;
            DecodedFrame::SupervisorRequest(SupervisorRequest::Status)
        }
        TAG_SV_REQ_QUERY => {
            let operation_id = r.bounded_string(MAX_OPERATION_ID_LEN, "query.operation_id", true)?;
            let operation_id = validate_operation_id("query.operation_id", operation_id)?;
            r.finish("query")?;
            DecodedFrame::SupervisorRequest(SupervisorRequest::Query { operation_id })
        }
        TAG_SV_REP_HELLO_OK => {
            let proto = r.u32("hello_ok.proto")?;
            let build = r.bounded_string(MAX_SUPERVISOR_STRING_LEN, "hello_ok.build", false)?;
            let pid = r.u32("hello_ok.pid")?;
            let created = r.u64("hello_ok.created")?;
            r.finish("hello_ok")?;
            DecodedFrame::SupervisorReply(SupervisorReply::HelloOk { proto, build, pid, created })
        }
        TAG_SV_REP_REFUSED => {
            let reason = SupervisorRefusedReason::try_from(r.u8("refused.reason")?)?;
            r.finish("refused")?;
            DecodedFrame::SupervisorReply(SupervisorReply::Refused { reason })
        }
        TAG_SV_REP_OPERATION => {
            let state = read_supervisor_operation_state(&mut r)?;
            r.finish("operation")?;
            DecodedFrame::SupervisorReply(SupervisorReply::Operation(state))
        }
        TAG_SV_REP_STATUS_OK => {
            let pid = r.u32("status_ok.pid")?;
            let created = r.u64("status_ok.created")?;
            let has_voyage = r.bool_flag("status_ok.has_voyage")?;
            let voyage = if has_voyage {
                Some(r.bounded_string(MAX_SUPERVISOR_STRING_LEN, "status_ok.voyage", true)?)
            } else {
                None
            };
            let has_leg = r.bool_flag("status_ok.has_leg")?;
            let leg = if has_leg { Some(r.u64("status_ok.leg")?) } else { None };
            let phase = SupervisorPhase::try_from(r.u8("status_ok.phase")?)?;
            r.finish("status_ok")?;
            DecodedFrame::SupervisorReply(SupervisorReply::StatusOk { pid, created, voyage, leg, phase })
        }
        other => return Err(WireError::UnknownTag(other)),
    };
    Ok(frame)
}
