//! The SOM0 (management) lane: its reason and state enums, frame types, encoders and body decoder.

use super::*;


/// `shutdown`'s `reason` byte-length bound.
pub const MAX_SHUTDOWN_REASON_LEN: usize = 128;
const _: () = assert!(MAX_SHUTDOWN_REASON_LEN <= u8::MAX as usize);

// ---------------------------------------------------------------------
// Closed reason/state enums
// ---------------------------------------------------------------------

/// `status_ok`'s survival field — supplied by the spawner, never inferred
/// (ADR 0041 decision 11); `Degraded` marks a breakaway-denied startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Survival {
    Normal = 0,
    Degraded = 1,
}

impl TryFrom<u8> for Survival {
    type Error = WireError;
    fn try_from(value: u8) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::Normal),
            1 => Ok(Self::Degraded),
            other => Err(WireError::UnknownEnumValue {
                field: "status_ok.survival",
                value: other,
            }),
        }
    }
}

// ---------------------------------------------------------------------
// Frame types
// ---------------------------------------------------------------------

/// Mgmt lane, client→server. Permanently pinned v0 shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MgmtRequest {
    Probe,
    Status,
    Shutdown { reason: String },
}

/// Mgmt lane, server→client. Permanently pinned v0 shapes — the reply
/// tag itself means success; there is no `ok` field anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MgmtReply {
    ProbeOk,
    StatusOk {
        pid: u32,
        created: u64,
        survival: Survival,
    },
    ShutdownOk,
}

// ---------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------

/// Encodes a mgmt-lane client→server frame as a complete `SOM0` wire
/// frame (header included), ready to write to the pipe.
pub fn encode_mgmt_request(frame: &MgmtRequest) -> Result<Vec<u8>, WireError> {
    let mut body = Vec::new();
    match frame {
        MgmtRequest::Probe => body.push(TAG_MGMT_REQ_PROBE),
        MgmtRequest::Status => body.push(TAG_MGMT_REQ_STATUS),
        MgmtRequest::Shutdown { reason } => {
            body.push(TAG_MGMT_REQ_SHUTDOWN);
            push_bounded_string(&mut body, reason, MAX_SHUTDOWN_REASON_LEN, "shutdown.reason", false)?;
        }
    }
    wrap(MGMT_MAGIC, body)
}

/// Encodes a mgmt-lane server→client frame as a complete `SOM0` wire
/// frame.
pub fn encode_mgmt_reply(frame: &MgmtReply) -> Result<Vec<u8>, WireError> {
    let mut body = Vec::new();
    match frame {
        MgmtReply::ProbeOk => body.push(TAG_MGMT_REP_PROBE_OK),
        MgmtReply::StatusOk {
            pid,
            created,
            survival,
        } => {
            body.push(TAG_MGMT_REP_STATUS_OK);
            push_u32(&mut body, *pid);
            push_u64(&mut body, *created);
            body.push(*survival as u8);
        }
        MgmtReply::ShutdownOk => body.push(TAG_MGMT_REP_SHUTDOWN_OK),
    }
    wrap(MGMT_MAGIC, body)
}

// ---------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------

pub(super) fn decode_mgmt_body(body: &[u8]) -> Result<DecodedFrame, WireError> {
    let mut r = Reader::new(body);
    let tag = r.u8("tag")?;
    let frame = match tag {
        TAG_MGMT_REQ_PROBE => {
            r.finish("probe")?;
            DecodedFrame::MgmtRequest(MgmtRequest::Probe)
        }
        TAG_MGMT_REQ_STATUS => {
            r.finish("status")?;
            DecodedFrame::MgmtRequest(MgmtRequest::Status)
        }
        TAG_MGMT_REQ_SHUTDOWN => {
            let reason = r.bounded_string(MAX_SHUTDOWN_REASON_LEN, "shutdown.reason", false)?;
            r.finish("shutdown")?;
            DecodedFrame::MgmtRequest(MgmtRequest::Shutdown { reason })
        }
        TAG_MGMT_REP_PROBE_OK => {
            r.finish("probe_ok")?;
            DecodedFrame::MgmtReply(MgmtReply::ProbeOk)
        }
        TAG_MGMT_REP_STATUS_OK => {
            let pid = r.u32("status_ok.pid")?;
            let created = r.u64("status_ok.created")?;
            let survival = Survival::try_from(r.u8("status_ok.survival")?)?;
            r.finish("status_ok")?;
            DecodedFrame::MgmtReply(MgmtReply::StatusOk {
                pid,
                created,
                survival,
            })
        }
        TAG_MGMT_REP_SHUTDOWN_OK => {
            r.finish("shutdown_ok")?;
            DecodedFrame::MgmtReply(MgmtReply::ShutdownOk)
        }
        other => return Err(WireError::UnknownTag(other)),
    };
    Ok(frame)
}
