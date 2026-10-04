//! The SOA0 (attach) lane: version negotiation, refusal reasons, frame types, checkpoint bounds, encoders and body decoder.

use super::*;


/// `controller_id`'s byte-length bound (`attach`, `take`, `input`).
/// Encode and decode also refuse a controller_id of length 0 — see
/// "Field minimums" in the module doc: an empty identity is malformed on
/// its face, unlike a reason string or a data payload.
pub const MAX_CONTROLLER_ID_LEN: usize = 128;
const _: () = assert!(MAX_CONTROLLER_ID_LEN <= u8::MAX as usize);

/// `input`'s `payload` byte-length bound — capped small on purpose (ADR
/// 0041 decision 3): it keeps the accepted blocking-`write_all` residual
/// from step 4 narrow rather than widening it to the 1 MiB frame cap.
pub const MAX_INPUT_PAYLOAD_LEN: usize = 8192;
const _: () = assert!(MAX_INPUT_PAYLOAD_LEN <= u16::MAX as usize);

/// The original attach-lane protocol version, still spoken by this
/// build for backward compatibility. `hello` refuses an incompatible
/// client before any checkpoint byte is ever generated, rather than
/// failing partway through a multi-MiB transfer.
///
/// Bound 1:1 to checkpoint format v1 (no scrollback ring) — see
/// [`ATTACH_PROTO_V2`]'s own doc for why that binding, once implicit, is
/// now enforced explicitly rather than merely documented.
pub const ATTACH_PROTO_V1: u32 = 1;

/// The current attach-lane protocol version (Codex round on #194,
/// finding 1 — "attach proto v2 bound to checkpoint v2").
///
/// A checkpoint reader tolerates every format version from
/// `rust/vt100/src/checkpoint::MIN_READABLE_VERSION` up
/// (`checkpoint::VERSION`, now 2, carries the scrollback ring) — but a
/// WRITER built before that ring existed does not: its own `VERSION`
/// constant is hardcoded to 1, so `Screen::restore` on that build
/// refuses a v2 payload outright, and that build's own pinned
/// `wire::MAX_CHECKPOINT_LEN` (8,651,327 B) predates the ring's larger
/// bound too, so even collecting an oversized transfer can fail before
/// restore is ever reached. Negotiating the OUTER attach-lane framing
/// version is what lets a capsule know, before it ever encodes a byte,
/// whether the peer on the other end can read a ring at all:
/// `ATTACH_PROTO_V2` promises checkpoint format v2 may follow;
/// `ATTACH_PROTO_V1` promises the capsule will encode v1 (no ring)
/// instead, regardless of how much scrollback the capsule itself keeps
/// live. `negotiate` accepts either from a client; which one a
/// connection settled on is what a capsule's `BeginCheckpoint` handling
/// reads back (`attach_proto::AttachProto::negotiated_proto`) to decide
/// which format version to encode.
pub const ATTACH_PROTO_V2: u32 = 2;

/// The current attach-lane protocol version (ADR 0046 decision 3, lane
/// B3b1): the capsule additionally emits, to a v3 watcher only,
/// [`AttachServer::PenSnapshot`]/[`AttachServer::PenChanged`]/
/// [`AttachServer::Geometry`] — the pen and geometry declared as the
/// voyage's own facts, never inferred by a watcher from output. Bound to
/// NOTHING new in the checkpoint format itself — `ATTACH_PROTO_V3`
/// promises these three extra event shapes MAY follow; checkpoint format
/// stays v2 either way (unlike the v1→v2 bump, this version does not
/// change what `BeginCheckpoint` encodes). v3 is REQUIRED for resident
/// service (ADR 0046 decision 3): a daemon relay refuses to serve a leg
/// that negotiated below it. `negotiate` accepts v1, v2, or v3 from a
/// client, echoing back exactly whichever one it asked for.
pub const ATTACH_PROTO_V3: u32 = 3;

/// The proven worst-case encoded size of a vt100-fork checkpoint (ADR
/// 0041 "Terminal state", step 3 as built, plus the scrollback ring
/// revision) — see the module doc for why this is a pinned literal rather
/// than a cross-crate reference.
pub const MAX_CHECKPOINT_LEN: usize = 12_030_729;

/// Fixed body overhead in a `checkpoint_chunk` frame ahead of its `bytes`:
/// the tag byte plus the `last` flag byte.
const CHECKPOINT_CHUNK_OVERHEAD: usize = 1 + 1;

/// The largest `bytes` payload one `checkpoint_chunk` frame can carry
/// while its whole body still satisfies [`MAX_BODY_LEN`].
pub const MAX_CHECKPOINT_CHUNK_PAYLOAD: usize = MAX_BODY_LEN - CHECKPOINT_CHUNK_OVERHEAD;

/// What a GREEDY encoder — one that always fills a `checkpoint_chunk` to
/// [`MAX_CHECKPOINT_CHUNK_PAYLOAD`] — produces for the worst-case
/// checkpoint: 11 full chunks of 1,048,574 B plus one 496,415 B chunk, 12
/// total. This is NOT a protocol maximum a decoder may enforce (see the
/// module doc's "Checkpoint chunk arithmetic" section) — it is what THIS
/// crate's own arithmetic test exercises, and the compile-time assertion
/// below fails loudly if either bound ever moves without the other.
pub const CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD: usize =
    MAX_CHECKPOINT_LEN.div_ceil(MAX_CHECKPOINT_CHUNK_PAYLOAD);
const _: () = assert!(CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD == 12);

/// `attach_refused`'s reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AttachRefusedReason {
    GroundTimeout = 0,
    SubscriberCap = 1,
}

impl TryFrom<u8> for AttachRefusedReason {
    type Error = WireError;
    fn try_from(value: u8) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::GroundTimeout),
            1 => Ok(Self::SubscriberCap),
            other => Err(WireError::UnknownEnumValue {
                field: "attach_refused.reason",
                value: other,
            }),
        }
    }
}

/// `take_refused`'s reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TakeRefusedReason {
    NotAttached = 0,
    CheckpointInFlight = 1,
}

impl TryFrom<u8> for TakeRefusedReason {
    type Error = WireError;
    fn try_from(value: u8) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::NotAttached),
            1 => Ok(Self::CheckpointInFlight),
            other => Err(WireError::UnknownEnumValue {
                field: "take_refused.reason",
                value: other,
            }),
        }
    }
}

/// `resize_refused`'s reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResizeRefusedReason {
    OutOfBudget = 0,
    NotDriver = 1,
}

impl TryFrom<u8> for ResizeRefusedReason {
    type Error = WireError;
    fn try_from(value: u8) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::OutOfBudget),
            1 => Ok(Self::NotDriver),
            other => Err(WireError::UnknownEnumValue {
                field: "resize_refused.reason",
                value: other,
            }),
        }
    }
}

// ---------------------------------------------------------------------
// Hello negotiation
// ---------------------------------------------------------------------

/// The outcome of negotiating an attach-lane protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Negotiated {
    /// The client's version is spoken; this is the version both sides use.
    Accepted(u32),
    /// The client's version is not spoken; `supported` is what this build
    /// speaks instead. The connection closes after this reply.
    Refused { supported: u32 },
}

/// Pure hello negotiation: this build's CAPSULE speaks [`ATTACH_PROTO_V1`],
/// [`ATTACH_PROTO_V2`] and [`ATTACH_PROTO_V3`], echoing back exactly
/// whichever one the client asked for (never silently upgrading it) —
/// called BEFORE any checkpoint byte is generated, so an incompatible
/// pair is refused here, not partway through a multi-MiB transfer. A
/// refusal reports the NEWEST version this build speaks, matching the
/// existing oldest-first-fallback shape a client already retries
/// through: a future, still-newer client refused here learns to try the
/// next older version down. ADR 0046 decision 3 (lane B3b1) is
/// CAPSULE-side only: `attach_worker::attach_lane_hello`'s own client
/// still asks for `ATTACH_PROTO_V2` first today (unchanged by this
/// lane), falling back to `ATTACH_PROTO_V1` against a pre-scrollback-ring
/// capsule — v3-first negotiation is a resident worker's job (B3b2),
/// added with its own first consumer.
#[must_use]
pub fn negotiate(client_proto: u32) -> Negotiated {
    if client_proto == ATTACH_PROTO_V1 || client_proto == ATTACH_PROTO_V2 || client_proto == ATTACH_PROTO_V3 {
        Negotiated::Accepted(client_proto)
    } else {
        Negotiated::Refused {
            supported: ATTACH_PROTO_V3,
        }
    }
}

/// Attach lane, client→server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachClient {
    Hello {
        proto: u32,
    },
    Attach {
        controller_id: String,
    },
    Take {
        controller_id: String,
    },
    Input {
        controller_id: String,
        take_epoch: u64,
        idem_key: [u8; 16],
        payload: Vec<u8>,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
}

/// Attach lane, server→client. No `attach_ok`: the first
/// [`CheckpointChunk`](AttachServer::CheckpointChunk) IS the attach
/// success signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachServer {
    HelloOk {
        proto: u32,
    },
    /// The server's spoken version; the connection closes after this.
    HelloRefused {
        supported: u32,
    },
    CheckpointChunk {
        last: bool,
        bytes: Vec<u8>,
    },
    AttachRefused {
        reason: AttachRefusedReason,
    },
    /// Post-fsync-watermark producer output.
    Output {
        bytes: Vec<u8>,
    },
    TakeOk {
        take_epoch: u64,
    },
    TakeRefused {
        reason: TakeRefusedReason,
    },
    /// Also the deterministic answer for a duplicate `idem_key` whose
    /// dedupe chain reached `forwarded`.
    InputRecorded,
    InputRefusedStale,
    /// A duplicate `idem_key` whose dedupe chain ends at `intent` — the
    /// caller MUST NOT auto-retry on this reply.
    InputDeliveryUnknown,
    ResizeOk,
    ResizeRefused {
        reason: ResizeRefusedReason,
    },
    /// ADR 0046 decision 3 (lane B3b1): a v3 watcher's OWN first v3
    /// event, always — sent right after that watcher's LAST checkpoint
    /// chunk, before any `PenChanged`. `holder` is the CURRENT driving
    /// connection's controller, or `None` if nobody currently holds the
    /// pen — this is the ephemeral, in-memory `AttachProto::driver`
    /// state, deliberately NOT the durable historical holder a resumed
    /// voyage's journal may still remember from a driver that has since
    /// disconnected (see `attach_proto`'s own doc on `DriverState` for
    /// why the two are distinct). `take_epoch` is `0` when `holder` is
    /// `None` — either because nobody has taken the pen yet this
    /// process's lifetime, OR because a previous driver already
    /// disconnected: the ephemeral state this reads cannot distinguish
    /// the two (losing the driver clears it with no durable trace, same
    /// as before this lane), so a watcher attaching after a disconnect
    /// sees the SAME `{None, 0}` a fresh voyage would, not the epoch a
    /// prior driver actually reached. v1/v2 watchers never receive this.
    PenSnapshot {
        holder: Option<String>,
        take_epoch: u64,
    },
    /// As `PenSnapshot`, sent to every v3 watcher whenever the pen
    /// changes hands: `Some(controller)` on a committed `take`, `None`
    /// when the driving connection disconnects (capability-only EOF —
    /// no durable transition, ADR 0041). Queued behind an in-flight
    /// checkpoint transfer for a watcher not yet `Done`, drained in
    /// order right after that watcher's own final chunk (never applied
    /// before `restore_screen`, so it can never be silently overwritten
    /// by the checkpoint's own baked-in state).
    PenChanged {
        holder: Option<String>,
        take_epoch: u64,
    },
    /// Sent to every v3 watcher after a successful resize — the SAME
    /// queue-behind-an-in-flight-checkpoint treatment as `PenChanged`
    /// (see its own doc): a watcher still mid-transfer never applies
    /// this before its own checkpoint's `restore_screen`, which would
    /// otherwise silently overwrite it with the checkpoint's own,
    /// possibly older, encoded dimensions.
    Geometry {
        cols: u16,
        rows: u16,
    },
}

/// Encodes an attach-lane client→server frame as a complete `SOA0` wire
/// frame.
pub fn encode_attach_client(frame: &AttachClient) -> Result<Vec<u8>, WireError> {
    let mut body = Vec::new();
    match frame {
        AttachClient::Hello { proto } => {
            body.push(TAG_ATTACH_REQ_HELLO);
            push_u32(&mut body, *proto);
        }
        AttachClient::Attach { controller_id } => {
            body.push(TAG_ATTACH_REQ_ATTACH);
            push_bounded_string(&mut body, controller_id, MAX_CONTROLLER_ID_LEN, "controller_id", true)?;
        }
        AttachClient::Take { controller_id } => {
            body.push(TAG_ATTACH_REQ_TAKE);
            push_bounded_string(&mut body, controller_id, MAX_CONTROLLER_ID_LEN, "controller_id", true)?;
        }
        AttachClient::Input {
            controller_id,
            take_epoch,
            idem_key,
            payload,
        } => {
            body.push(TAG_ATTACH_REQ_INPUT);
            push_bounded_string(&mut body, controller_id, MAX_CONTROLLER_ID_LEN, "controller_id", true)?;
            push_u64(&mut body, *take_epoch);
            body.extend_from_slice(idem_key);
            push_bounded_bytes(&mut body, payload, MAX_INPUT_PAYLOAD_LEN, "input.payload")?;
        }
        AttachClient::Resize { cols, rows } => {
            body.push(TAG_ATTACH_REQ_RESIZE);
            push_u16(&mut body, *cols);
            push_u16(&mut body, *rows);
        }
    }
    wrap(ATTACH_MAGIC, body)
}

/// Encodes the single `keepalive` frame (tag `0x06`) as a complete `SOA0`
/// wire frame. There is exactly one shape: the server originates it and
/// the client echoes the identical bytes back, so "encoding it as the
/// client" and "encoding it as the server" must be indistinguishable —
/// one function, not one per direction (see the module doc's tag-table
/// note — a review round caught a two-tag design breaking exactly this
/// verbatim-echo requirement).
pub fn encode_keepalive(nonce: u64) -> Vec<u8> {
    let mut body = vec![TAG_ATTACH_KEEPALIVE];
    push_u64(&mut body, nonce);
    // A fixed 9-byte body is always within MAX_BODY_LEN; `wrap` cannot
    // fail here, but the shared helper still returns `Result` for
    // callers that build a body from unbounded fields.
    wrap(ATTACH_MAGIC, body).expect("fixed-size keepalive body never exceeds the cap")
}

/// Encodes an attach-lane server→client frame as a complete `SOA0` wire
/// frame.
pub fn encode_attach_server(frame: &AttachServer) -> Result<Vec<u8>, WireError> {
    let mut body = Vec::new();
    match frame {
        AttachServer::HelloOk { proto } => {
            body.push(TAG_ATTACH_REP_HELLO_OK);
            push_u32(&mut body, *proto);
        }
        AttachServer::HelloRefused { supported } => {
            body.push(TAG_ATTACH_REP_HELLO_REFUSED);
            push_u32(&mut body, *supported);
        }
        AttachServer::CheckpointChunk { last, bytes } => {
            if bytes.len() > MAX_CHECKPOINT_CHUNK_PAYLOAD {
                return Err(WireError::FieldTooLarge {
                    field: "checkpoint_chunk.bytes",
                    len: bytes.len(),
                    max: MAX_CHECKPOINT_CHUNK_PAYLOAD,
                });
            }
            body.push(TAG_ATTACH_REP_CHECKPOINT_CHUNK);
            body.push(u8::from(*last));
            body.extend_from_slice(bytes);
        }
        AttachServer::AttachRefused { reason } => {
            body.push(TAG_ATTACH_REP_ATTACH_REFUSED);
            body.push(*reason as u8);
        }
        AttachServer::Output { bytes } => {
            const MAX_OUTPUT_PAYLOAD: usize = MAX_BODY_LEN - 1;
            if bytes.len() > MAX_OUTPUT_PAYLOAD {
                return Err(WireError::FieldTooLarge {
                    field: "output.bytes",
                    len: bytes.len(),
                    max: MAX_OUTPUT_PAYLOAD,
                });
            }
            body.push(TAG_ATTACH_REP_OUTPUT);
            body.extend_from_slice(bytes);
        }
        AttachServer::TakeOk { take_epoch } => {
            body.push(TAG_ATTACH_REP_TAKE_OK);
            push_u64(&mut body, *take_epoch);
        }
        AttachServer::TakeRefused { reason } => {
            body.push(TAG_ATTACH_REP_TAKE_REFUSED);
            body.push(*reason as u8);
        }
        AttachServer::InputRecorded => body.push(TAG_ATTACH_REP_INPUT_RECORDED),
        AttachServer::InputRefusedStale => body.push(TAG_ATTACH_REP_INPUT_REFUSED_STALE),
        AttachServer::InputDeliveryUnknown => body.push(TAG_ATTACH_REP_INPUT_DELIVERY_UNKNOWN),
        AttachServer::ResizeOk => body.push(TAG_ATTACH_REP_RESIZE_OK),
        AttachServer::ResizeRefused { reason } => {
            body.push(TAG_ATTACH_REP_RESIZE_REFUSED);
            body.push(*reason as u8);
        }
        AttachServer::PenSnapshot { holder, take_epoch } => {
            body.push(TAG_ATTACH_REP_PEN_SNAPSHOT);
            push_holder(&mut body, holder)?;
            push_u64(&mut body, *take_epoch);
        }
        AttachServer::PenChanged { holder, take_epoch } => {
            body.push(TAG_ATTACH_REP_PEN_CHANGED);
            push_holder(&mut body, holder)?;
            push_u64(&mut body, *take_epoch);
        }
        AttachServer::Geometry { cols, rows } => {
            body.push(TAG_ATTACH_REP_GEOMETRY);
            push_u16(&mut body, *cols);
            push_u16(&mut body, *rows);
        }
    }
    wrap(ATTACH_MAGIC, body)
}

pub(super) fn decode_attach_body(body: &[u8]) -> Result<DecodedFrame, WireError> {
    let mut r = Reader::new(body);
    let tag = r.u8("tag")?;
    let frame = match tag {
        TAG_ATTACH_REQ_HELLO => {
            let proto = r.u32("hello.proto")?;
            r.finish("hello")?;
            DecodedFrame::AttachClient(AttachClient::Hello { proto })
        }
        TAG_ATTACH_REQ_ATTACH => {
            let controller_id = r.bounded_string(MAX_CONTROLLER_ID_LEN, "controller_id", true)?;
            r.finish("attach")?;
            DecodedFrame::AttachClient(AttachClient::Attach { controller_id })
        }
        TAG_ATTACH_REQ_TAKE => {
            let controller_id = r.bounded_string(MAX_CONTROLLER_ID_LEN, "controller_id", true)?;
            r.finish("take")?;
            DecodedFrame::AttachClient(AttachClient::Take { controller_id })
        }
        TAG_ATTACH_REQ_INPUT => {
            let controller_id = r.bounded_string(MAX_CONTROLLER_ID_LEN, "controller_id", true)?;
            let take_epoch = r.u64("input.take_epoch")?;
            let idem_key = r.array16("input.idem_key")?;
            let payload = r.bounded_bytes(MAX_INPUT_PAYLOAD_LEN, "input.payload")?;
            r.finish("input")?;
            DecodedFrame::AttachClient(AttachClient::Input {
                controller_id,
                take_epoch,
                idem_key,
                payload,
            })
        }
        TAG_ATTACH_REQ_RESIZE => {
            let cols = r.u16("resize.cols")?;
            let rows = r.u16("resize.rows")?;
            r.finish("resize")?;
            DecodedFrame::AttachClient(AttachClient::Resize { cols, rows })
        }
        TAG_ATTACH_KEEPALIVE => {
            let nonce = r.u64("keepalive.nonce")?;
            r.finish("keepalive")?;
            DecodedFrame::Keepalive { nonce }
        }
        TAG_ATTACH_REP_HELLO_OK => {
            let proto = r.u32("hello_ok.proto")?;
            r.finish("hello_ok")?;
            DecodedFrame::AttachServer(AttachServer::HelloOk { proto })
        }
        TAG_ATTACH_REP_HELLO_REFUSED => {
            let supported = r.u32("hello_refused.supported")?;
            r.finish("hello_refused")?;
            DecodedFrame::AttachServer(AttachServer::HelloRefused { supported })
        }
        TAG_ATTACH_REP_CHECKPOINT_CHUNK => {
            let last = r.bool_flag("checkpoint_chunk.last")?;
            let bytes = r.rest();
            DecodedFrame::AttachServer(AttachServer::CheckpointChunk { last, bytes })
        }
        TAG_ATTACH_REP_ATTACH_REFUSED => {
            let reason = AttachRefusedReason::try_from(r.u8("attach_refused.reason")?)?;
            r.finish("attach_refused")?;
            DecodedFrame::AttachServer(AttachServer::AttachRefused { reason })
        }
        TAG_ATTACH_REP_OUTPUT => {
            let bytes = r.rest();
            DecodedFrame::AttachServer(AttachServer::Output { bytes })
        }
        TAG_ATTACH_REP_TAKE_OK => {
            let take_epoch = r.u64("take_ok.take_epoch")?;
            r.finish("take_ok")?;
            DecodedFrame::AttachServer(AttachServer::TakeOk { take_epoch })
        }
        TAG_ATTACH_REP_TAKE_REFUSED => {
            let reason = TakeRefusedReason::try_from(r.u8("take_refused.reason")?)?;
            r.finish("take_refused")?;
            DecodedFrame::AttachServer(AttachServer::TakeRefused { reason })
        }
        TAG_ATTACH_REP_INPUT_RECORDED => {
            r.finish("input_recorded")?;
            DecodedFrame::AttachServer(AttachServer::InputRecorded)
        }
        TAG_ATTACH_REP_INPUT_REFUSED_STALE => {
            r.finish("input_refused_stale")?;
            DecodedFrame::AttachServer(AttachServer::InputRefusedStale)
        }
        TAG_ATTACH_REP_INPUT_DELIVERY_UNKNOWN => {
            r.finish("input_delivery_unknown")?;
            DecodedFrame::AttachServer(AttachServer::InputDeliveryUnknown)
        }
        TAG_ATTACH_REP_RESIZE_OK => {
            r.finish("resize_ok")?;
            DecodedFrame::AttachServer(AttachServer::ResizeOk)
        }
        TAG_ATTACH_REP_RESIZE_REFUSED => {
            let reason = ResizeRefusedReason::try_from(r.u8("resize_refused.reason")?)?;
            r.finish("resize_refused")?;
            DecodedFrame::AttachServer(AttachServer::ResizeRefused { reason })
        }
        TAG_ATTACH_REP_PEN_SNAPSHOT => {
            let holder = r.holder("pen_snapshot.holder")?;
            let take_epoch = r.u64("pen_snapshot.take_epoch")?;
            r.finish("pen_snapshot")?;
            DecodedFrame::AttachServer(AttachServer::PenSnapshot { holder, take_epoch })
        }
        TAG_ATTACH_REP_PEN_CHANGED => {
            let holder = r.holder("pen_changed.holder")?;
            let take_epoch = r.u64("pen_changed.take_epoch")?;
            r.finish("pen_changed")?;
            DecodedFrame::AttachServer(AttachServer::PenChanged { holder, take_epoch })
        }
        TAG_ATTACH_REP_GEOMETRY => {
            let cols = r.u16("geometry.cols")?;
            let rows = r.u16("geometry.rows")?;
            r.finish("geometry")?;
            DecodedFrame::AttachServer(AttachServer::Geometry { cols, rows })
        }
        other => return Err(WireError::UnknownTag(other)),
    };
    Ok(frame)
}
