//! The ADR 0041 step-5 pipe protocol: outer framing, lane binding, and
//! every frame layout for both pipe lanes, as pure encode/decode.
//!
//! This module is bytes in, typed frames out — and typed frames in, bytes
//! out — for both directions of both lanes. It has no I/O, no clocks, no
//! role/permission machine, and no keepalive timers: the ADR 0041 "Step 5
//! as specified" pre-implementation review moved those to other units
//! (the capsule's writer loop and the Windows transport) and pinned this
//! module's scope to the wire alone. `host_handshake.rs` is this crate's
//! precedent for a platform-neutral, no-I/O byte-state-machine module;
//! this one follows its documentation and test discipline.
//!
//! # Outer framing
//!
//! ```text
//! magic   4   b"SOM0" (mgmt lane, permanently pinned v0) or
//!             b"SOA0" (attach lane, versioned via hello)
//! len     4   u32-LE, the BODY length in bytes, capped at 1 MiB
//! body  len   lane-specific, below
//! ```
//!
//! The first frame's magic BINDS the connection's lane; a later frame
//! whose magic differs is a protocol error ([`WireError::LaneMismatch`]),
//! and a magic that is neither `SOM0` nor `SOA0`, at any position, is
//! [`WireError::UnknownMagic`]. [`FrameSplitter`] is where this is
//! enforced — it is a framing property, not a per-message one. `len`
//! exceeding [`MAX_BODY_LEN`] is checked the instant the 8-byte header is
//! available, before the splitter ever looks for that many body bytes —
//! there is no speculative body buffer sized from an untrusted `len` to
//! allocate in the first place. A header announcing a valid `len` whose
//! body has not fully arrived yet is NOT an error: the splitter carries
//! the partial frame across `feed` calls, tolerating a chunk cut at any
//! byte boundary, the same discipline `host_handshake.rs` uses.
//!
//! All multi-byte integers are little-endian. Every body is fixed binary
//! — no JSON, no base64 — so input, output, and checkpoint bytes ride raw
//! and the chunk arithmetic (see [`MAX_CHECKPOINT_LEN`] below) is exact.
//!
//! # One tag-byte scheme, two lanes
//!
//! Every frame body starts with a one-byte tag. Within EACH lane, tags
//! below `0x80` are client→server (requests) and tags with the `0x80` bit
//! set are server→client (replies/pushes) — the same shape in both lanes,
//! which is what makes the scheme "coherent" rather than two unrelated
//! ones. The two lanes are independent decode contexts (the outer magic
//! already resolves which table applies before the tag byte is even
//! read), so a numeric tag value is reused between `SOM0` and `SOA0`
//! bodies below purely because each lane restarts its own opcode space —
//! never two meanings sharing one value inside the same lane.
//!
//! | lane | dir | tag    | frame                    | body after the tag |
//! |------|-----|--------|--------------------------|---------------------|
//! | SOM0 | C→S | `0x01` | `probe`                  | (none) |
//! | SOM0 | C→S | `0x02` | `status`                 | (none) |
//! | SOM0 | C→S | `0x03` | `shutdown`               | `reason`: len u8 + UTF-8, ≤128 B |
//! | SOM0 | S→C | `0x81` | `probe_ok`               | (none) |
//! | SOM0 | S→C | `0x82` | `status_ok`              | `pid`:u32-LE, `created`:u64-LE, `survival`:u8 |
//! | SOM0 | S→C | `0x83` | `shutdown_ok`            | (none) |
//! | SOA0 | C→S | `0x01` | `hello`                  | `proto`:u32-LE |
//! | SOA0 | C→S | `0x02` | `attach`                 | `controller_id`: len u8 + UTF-8, ≤128 B |
//! | SOA0 | C→S | `0x03` | `take`                   | `controller_id` (same shape) |
//! | SOA0 | C→S | `0x04` | `input`                  | `controller_id`, `take_epoch`:u64-LE, `idem_key`:[u8;16], `payload`: len u16-LE + bytes, ≤8192 B |
//! | SOA0 | C→S | `0x05` | `resize`                 | `cols`:u16-LE, `rows`:u16-LE |
//! | SOA0 | ↔   | `0x06` | `keepalive`              | `nonce`:u64-LE — ONE shape, both directions |
//! | SOA0 | S→C | `0x81` | `hello_ok`               | `proto`:u32-LE |
//! | SOA0 | S→C | `0x82` | `hello_refused`          | `supported`:u32-LE |
//! | SOA0 | S→C | `0x83` | `checkpoint_chunk`       | `last`:u8 (0/1), `bytes`: rest of body |
//! | SOA0 | S→C | `0x84` | `attach_refused`         | `reason`:u8 (closed enum) |
//! | SOA0 | S→C | `0x85` | `output`                 | `bytes`: rest of body |
//! | SOA0 | S→C | `0x86` | `take_ok`                | `take_epoch`:u64-LE |
//! | SOA0 | S→C | `0x87` | `take_refused`           | `reason`:u8 (closed enum) |
//! | SOA0 | S→C | `0x88` | `input_recorded`         | (none) |
//! | SOA0 | S→C | `0x89` | `input_refused_stale`    | (none) |
//! | SOA0 | S→C | `0x8a` | `input_delivery_unknown` | (none) |
//! | SOA0 | S→C | `0x8b` | `resize_ok`              | (none) |
//! | SOA0 | S→C | `0x8c` | `resize_refused`         | `reason`:u8 (closed enum) |
//!
//! There is deliberately no `attach_ok`: the first `checkpoint_chunk` IS
//! the attach success signal (one fewer frame type). Both lanes are
//! lockstep per connection — one outstanding client request at a time, no
//! correlation IDs anywhere — which is a rule the CALLER enforces; this
//! module only defines what a frame looks like.
//!
//! One exception to the direction-by-high-bit rule: `keepalive` (tag
//! `0x06`, listed once above) is DIRECTION-NEUTRAL. The ADR pins a single
//! echo frame — the server originates it, and the client's reply is
//! required to be the identical bytes bounced back — so it cannot have
//! one tag per direction without breaking that verbatim-echo requirement.
//! (An earlier draft of this module used two tags, `0x06` and `0x8d`; a
//! review round caught that a "verbatim echo" of the server's `0x8d`
//! frame cannot decode back in as a *client* frame under a two-tag
//! scheme. The fix is what ships: one shape, one tag, [`encode_keepalive`]
//! the only encoder, decoding to the direction-neutral
//! [`DecodedFrame::Keepalive`].)
//!
//! # Field minimums
//!
//! Every length-prefixed field defaults to a maximum only. `controller_id`
//! is the one exception: it must be at least 1 byte, rejected at both
//! encode and decode ([`WireError::FieldEmpty`]) — an empty identity is
//! malformed on its face, not a legitimate degenerate case. Every other
//! variable-length field — `shutdown`'s `reason`, `input`'s `payload`,
//! `output`'s `bytes`, and a non-final `checkpoint_chunk`'s `bytes` — is
//! legally empty: "nothing to say this round" is a real state for a
//! reason string or a data payload, unlike an identity.
//!
//! # Types
//!
//! Four frame enums, one per (lane, direction): [`MgmtRequest`] /
//! [`MgmtReply`] for `SOM0`, [`AttachClient`] / [`AttachServer`] for
//! `SOA0`. Each has its own `encode_*` function, so encoding a
//! server-only frame from client code (or vice versa) requires calling
//! the wrong function by name — not a mistake the type system makes for
//! you, but not a silent one either. `keepalive` is the one exception
//! (see the tag-table note above): it is neither an `AttachClient` nor an
//! `AttachServer` variant — only [`DecodedFrame::Keepalive`], produced by
//! the single [`encode_keepalive`] function regardless of which side
//! calls it. [`FrameSplitter::feed`] decodes whichever shape a body's
//! lane and tag identify, wrapped in [`DecodedFrame`].
//!
//! # Errors
//!
//! [`WireError`] distinguishes what a caller can do about a failure:
//! magic/lane problems are connection-framing errors; everything else is
//! a malformed body the caller treats as connection-fatal (per the ADR).
//! [`FrameSplitter::feed`] returns `(frames, Option<WireError>)` rather
//! than a `Result`, because frames decoded earlier in the SAME call must
//! never be silently dropped just because a later one in that call
//! failed. Once an error occurs the splitter LATCHES failed: its buffer
//! is freed immediately, and every subsequent `feed` call returns the
//! identical error at no cost, regardless of what bytes it is given —
//! the connection-fatal contract is enforced here, not merely documented.
//!
//! # Checkpoint chunk arithmetic
//!
//! The vt100 fork's worst-case encoded checkpoint (both grids at the
//! ADR 0041 maximum 512×256 geometry, `rust/vt100/src/checkpoint.rs`'s
//! `MAX_CHECKPOINT_LEN`) is a PROVEN 8,651,327 bytes. This module pins
//! that number as [`MAX_CHECKPOINT_LEN`] rather than depending on the
//! `vt100` crate: the checkpoint's bytes are opaque to the wire (they
//! ride inside `checkpoint_chunk` exactly like any other payload), and
//! that crate is a Windows-only dependency of this one today, while this
//! module — like `host_handshake.rs` — builds and is tested on every
//! platform. If the fork's format ever changes, this literal must move
//! with it; nothing here computes it independently.
//! [`MAX_CHECKPOINT_CHUNK_PAYLOAD`] is the largest `bytes` a single
//! `checkpoint_chunk` can carry within the outer [`MAX_BODY_LEN`] cap.
//! [`CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD`] is what a GREEDY encoder (one
//! that always fills a chunk to that payload bound) produces for the
//! worst-case checkpoint — 9 — but it is NOT a protocol maximum: nothing
//! on the wire counts or caps `checkpoint_chunk` frames, and a sender
//! using smaller chunks may legally emit more of them, including empty
//! non-final ones. A decoder must never reject a stream for having "too
//! many" chunks. Bounding the REASSEMBLED total (summed `bytes` across
//! every chunk, checked against [`MAX_CHECKPOINT_LEN`]) is the
//! CONSUMER's job — this module only ever bounds one frame's bytes
//! against [`MAX_BODY_LEN`], never a running total across frames.

/// The mgmt lane's magic (`SOM0`) — permanently pinned v0 framing, never
/// versioned. A connection whose first frame carries this magic is
/// mgmt-typed for its whole lifetime.
pub const MGMT_MAGIC: [u8; 4] = *b"SOM0";

/// The attach lane's magic (`SOA0`) — versioned via `hello` above this
/// framing, which itself never changes.
pub const ATTACH_MAGIC: [u8; 4] = *b"SOA0";

/// Bytes in the outer header (`magic` + `len`), ahead of the body.
const HEADER_LEN: usize = 4 + 4;

/// The body-length cap (1 MiB), enforced before the splitter looks for
/// that many body bytes.
pub const MAX_BODY_LEN: usize = 1_048_576;

/// `controller_id`'s byte-length bound (`attach`, `take`, `input`).
/// Encode and decode also refuse a controller_id of length 0 — see
/// "Field minimums" in the module doc: an empty identity is malformed on
/// its face, unlike a reason string or a data payload.
pub const MAX_CONTROLLER_ID_LEN: usize = 128;
const _: () = assert!(MAX_CONTROLLER_ID_LEN <= u8::MAX as usize);

/// `shutdown`'s `reason` byte-length bound.
pub const MAX_SHUTDOWN_REASON_LEN: usize = 128;
const _: () = assert!(MAX_SHUTDOWN_REASON_LEN <= u8::MAX as usize);

/// `input`'s `payload` byte-length bound — capped small on purpose (ADR
/// 0041 decision 3): it keeps the accepted blocking-`write_all` residual
/// from step 4 narrow rather than widening it to the 1 MiB frame cap.
pub const MAX_INPUT_PAYLOAD_LEN: usize = 8192;
const _: () = assert!(MAX_INPUT_PAYLOAD_LEN <= u16::MAX as usize);

/// The only attach-lane protocol version this build speaks. Attach proto
/// v1 binds checkpoint format v1 (`rust/vt100/src/checkpoint::VERSION`) —
/// that binding is why `hello` can refuse before any checkpoint byte is
/// ever generated, rather than failing partway through a multi-MiB
/// transfer.
pub const ATTACH_PROTO_V1: u32 = 1;

/// The proven worst-case encoded size of a vt100-fork checkpoint (ADR
/// 0041 "Terminal state", step 3 as built) — see the module doc for why
/// this is a pinned literal rather than a cross-crate reference.
pub const MAX_CHECKPOINT_LEN: usize = 8_651_327;

/// Fixed body overhead in a `checkpoint_chunk` frame ahead of its `bytes`:
/// the tag byte plus the `last` flag byte.
const CHECKPOINT_CHUNK_OVERHEAD: usize = 1 + 1;

/// The largest `bytes` payload one `checkpoint_chunk` frame can carry
/// while its whole body still satisfies [`MAX_BODY_LEN`].
pub const MAX_CHECKPOINT_CHUNK_PAYLOAD: usize = MAX_BODY_LEN - CHECKPOINT_CHUNK_OVERHEAD;

/// What a GREEDY encoder — one that always fills a `checkpoint_chunk` to
/// [`MAX_CHECKPOINT_CHUNK_PAYLOAD`] — produces for the worst-case
/// checkpoint: 8 full chunks of 1,048,574 B plus one 262,735 B chunk, 9
/// total. This is NOT a protocol maximum a decoder may enforce (see the
/// module doc's "Checkpoint chunk arithmetic" section) — it is what THIS
/// crate's own arithmetic test exercises, and the compile-time assertion
/// below fails loudly if either bound ever moves without the other.
pub const CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD: usize =
    MAX_CHECKPOINT_LEN.div_ceil(MAX_CHECKPOINT_CHUNK_PAYLOAD);
const _: () = assert!(CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD == 9);

// ---------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------

const TAG_MGMT_REQ_PROBE: u8 = 0x01;
const TAG_MGMT_REQ_STATUS: u8 = 0x02;
const TAG_MGMT_REQ_SHUTDOWN: u8 = 0x03;
const TAG_MGMT_REP_PROBE_OK: u8 = 0x81;
const TAG_MGMT_REP_STATUS_OK: u8 = 0x82;
const TAG_MGMT_REP_SHUTDOWN_OK: u8 = 0x83;

const TAG_ATTACH_REQ_HELLO: u8 = 0x01;
const TAG_ATTACH_REQ_ATTACH: u8 = 0x02;
const TAG_ATTACH_REQ_TAKE: u8 = 0x03;
const TAG_ATTACH_REQ_INPUT: u8 = 0x04;
const TAG_ATTACH_REQ_RESIZE: u8 = 0x05;
/// `keepalive` — direction-neutral, legal (and byte-identical) whichever
/// side sends it. See the module doc's tag-table note.
const TAG_ATTACH_KEEPALIVE: u8 = 0x06;
const TAG_ATTACH_REP_HELLO_OK: u8 = 0x81;
const TAG_ATTACH_REP_HELLO_REFUSED: u8 = 0x82;
const TAG_ATTACH_REP_CHECKPOINT_CHUNK: u8 = 0x83;
const TAG_ATTACH_REP_ATTACH_REFUSED: u8 = 0x84;
const TAG_ATTACH_REP_OUTPUT: u8 = 0x85;
const TAG_ATTACH_REP_TAKE_OK: u8 = 0x86;
const TAG_ATTACH_REP_TAKE_REFUSED: u8 = 0x87;
const TAG_ATTACH_REP_INPUT_RECORDED: u8 = 0x88;
const TAG_ATTACH_REP_INPUT_REFUSED_STALE: u8 = 0x89;
const TAG_ATTACH_REP_INPUT_DELIVERY_UNKNOWN: u8 = 0x8a;
const TAG_ATTACH_REP_RESIZE_OK: u8 = 0x8b;
const TAG_ATTACH_REP_RESIZE_REFUSED: u8 = 0x8c;

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// Why a frame could not be encoded or decoded.
///
/// Every variant is something a caller can act on: the first three are
/// framing/connection problems ([`FrameSplitter`] catches these before any
/// body is even parsed); the rest are a malformed body, which the ADR
/// treats as connection-fatal regardless of which one it is. None of these
/// is ever raised for a body that simply hasn't fully arrived yet — that
/// case is not an error at all, it is carry (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// Neither `SOM0` nor `SOA0` — no lane recognizes this magic.
    #[error("unrecognized frame magic {0:?} (expected SOM0 or SOA0)")]
    UnknownMagic([u8; 4]),
    /// This connection's lane was already latched by an earlier frame's
    /// magic; this frame's magic does not match it.
    #[error(
        "frame magic {got:?} does not match this connection's lane, latched as {latched:?} by its first frame"
    )]
    LaneMismatch { latched: [u8; 4], got: [u8; 4] },
    /// The header announced a body length over [`MAX_BODY_LEN`]. Checked
    /// immediately after the header is available, before any attempt to
    /// gather that many body bytes.
    #[error("frame body length {0} exceeds the 1 MiB cap")]
    BodyTooLarge(u32),
    /// The body ended before a field its tag says must be there. This is
    /// never a body the outer framing considers incomplete (that carries,
    /// see the module doc) — this is a fully-received body whose declared
    /// internal shape does not fit the bytes it actually has.
    #[error("malformed frame body: {0}")]
    Malformed(&'static str),
    /// A fully-parsed, fixed-shape body carried extra bytes past what its
    /// tag defines.
    #[error("frame body carried trailing bytes past {0}")]
    TrailingBytes(&'static str),
    /// The tag byte is not defined for this lane.
    #[error("unknown frame tag {0:#04x} for this lane")]
    UnknownTag(u8),
    /// A length-prefixed field's bytes are not valid UTF-8.
    #[error("field {0} is not valid UTF-8")]
    InvalidUtf8(&'static str),
    /// A closed-enum byte (a reason code, `survival`, `checkpoint_chunk`'s
    /// `last` flag) held a value outside its named constants.
    #[error("field {field} has unrecognized value {value}")]
    UnknownEnumValue { field: &'static str, value: u8 },
    /// A length-prefixed field's declared length exceeds this protocol's
    /// bound for that field (distinct from the outer 1 MiB cap).
    #[error("field {field} length {len} exceeds the protocol bound of {max} bytes")]
    FieldTooLarge {
        field: &'static str,
        len: usize,
        max: usize,
    },
    /// A field that must not be empty (`controller_id` — see "Field
    /// minimums" in the module doc) was zero bytes.
    #[error("field {0} must not be empty")]
    FieldEmpty(&'static str),
}

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

/// Pure hello negotiation: v1 is the only version this build speaks.
/// Called BEFORE any checkpoint byte is generated — an incompatible pair
/// is refused here, not partway through a multi-MiB transfer, because
/// attach proto v1 binds checkpoint format v1.
#[must_use]
pub fn negotiate(client_proto: u32) -> Negotiated {
    if client_proto == ATTACH_PROTO_V1 {
        Negotiated::Accepted(ATTACH_PROTO_V1)
    } else {
        Negotiated::Refused {
            supported: ATTACH_PROTO_V1,
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
}

/// What [`FrameSplitter::feed`] decoded a body into — the lane and
/// direction the tag byte identified, EXCEPT `Keepalive`: the ADR pins
/// one echo frame, server-originated and bounced back byte-identical by
/// the client, so it is neither an `AttachClient` nor an `AttachServer`
/// variant — see the module doc's tag-table note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedFrame {
    MgmtRequest(MgmtRequest),
    MgmtReply(MgmtReply),
    AttachClient(AttachClient),
    AttachServer(AttachServer),
    Keepalive { nonce: u64 },
}

// ---------------------------------------------------------------------
// Byte-level helpers
// ---------------------------------------------------------------------

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Appends `s` as a `len:u8 + UTF-8 bytes` field, refusing (rather than
/// truncating or panicking) if it is longer than `max` — or, when
/// `require_nonempty` is set (`controller_id`; see "Field minimums" in
/// the module doc), if it is empty.
fn push_bounded_string(
    out: &mut Vec<u8>,
    s: &str,
    max: usize,
    field: &'static str,
    require_nonempty: bool,
) -> Result<(), WireError> {
    let bytes = s.as_bytes();
    if require_nonempty && bytes.is_empty() {
        return Err(WireError::FieldEmpty(field));
    }
    if bytes.len() > max {
        return Err(WireError::FieldTooLarge {
            field,
            len: bytes.len(),
            max,
        });
    }
    // Safe: `max <= u8::MAX` is asserted at the const site for every
    // caller of this helper.
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
    Ok(())
}

/// Appends `bytes` as a `len:u16-LE + bytes` field, refusing if longer
/// than `max`.
fn push_bounded_bytes(
    out: &mut Vec<u8>,
    bytes: &[u8],
    max: usize,
    field: &'static str,
) -> Result<(), WireError> {
    if bytes.len() > max {
        return Err(WireError::FieldTooLarge {
            field,
            len: bytes.len(),
            max,
        });
    }
    // Safe: `max <= u16::MAX` is asserted at the const site for every
    // caller of this helper.
    push_u16(out, bytes.len() as u16);
    out.extend_from_slice(bytes);
    Ok(())
}

/// Wraps a body in the outer `magic + len` header, refusing bodies over
/// [`MAX_BODY_LEN`] rather than producing a frame no splitter could ever
/// read back.
fn wrap(magic: [u8; 4], body: Vec<u8>) -> Result<Vec<u8>, WireError> {
    if body.len() > MAX_BODY_LEN {
        let len_for_error = u32::try_from(body.len()).unwrap_or(u32::MAX);
        return Err(WireError::BodyTooLarge(len_for_error));
    }
    // Safe: checked above against MAX_BODY_LEN, which fits in u32.
    let len = body.len() as u32;
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&magic);
    push_u32(&mut out, len);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Bounds-checked cursor over one already-length-known frame body. Every
/// accessor returns [`WireError`] rather than panicking or reading out of
/// bounds, so decoding arbitrary bytes is safe by construction.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize, field: &'static str) -> Result<&'a [u8], WireError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.buf.len())
            .ok_or(WireError::Malformed(field))?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self, field: &'static str) -> Result<u8, WireError> {
        Ok(self.take(1, field)?[0])
    }

    fn u16(&mut self, field: &'static str) -> Result<u16, WireError> {
        let b = self.take(2, field)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, field: &'static str) -> Result<u32, WireError> {
        let b = self.take(4, field)?;
        Ok(u32::from_le_bytes(b.try_into().expect("checked len 4")))
    }

    fn u64(&mut self, field: &'static str) -> Result<u64, WireError> {
        let b = self.take(8, field)?;
        Ok(u64::from_le_bytes(b.try_into().expect("checked len 8")))
    }

    fn array16(&mut self, field: &'static str) -> Result<[u8; 16], WireError> {
        let b = self.take(16, field)?;
        Ok(b.try_into().expect("checked len 16"))
    }

    /// Reads a byte that must be exactly 0 or 1, refusing any other value
    /// rather than treating it as a permissive `!= 0` boolean.
    fn bool_flag(&mut self, field: &'static str) -> Result<bool, WireError> {
        match self.u8(field)? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(WireError::UnknownEnumValue { field, value: other }),
        }
    }

    fn bounded_string(
        &mut self,
        max: usize,
        field: &'static str,
        require_nonempty: bool,
    ) -> Result<String, WireError> {
        let len = usize::from(self.u8(field)?);
        if require_nonempty && len == 0 {
            return Err(WireError::FieldEmpty(field));
        }
        if len > max {
            return Err(WireError::FieldTooLarge { field, len, max });
        }
        let bytes = self.take(len, field)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| WireError::InvalidUtf8(field))
    }

    fn bounded_bytes(&mut self, max: usize, field: &'static str) -> Result<Vec<u8>, WireError> {
        let len = usize::from(self.u16(field)?);
        if len > max {
            return Err(WireError::FieldTooLarge { field, len, max });
        }
        Ok(self.take(len, field)?.to_vec())
    }

    /// Consumes and returns every remaining byte (`checkpoint_chunk` and
    /// `output`'s trailing raw payload, whose length is implicit in the
    /// outer frame length rather than a separate prefix).
    fn rest(&mut self) -> Vec<u8> {
        let out = self.buf[self.pos..].to_vec();
        self.pos = self.buf.len();
        out
    }

    fn finish(self, field: &'static str) -> Result<(), WireError> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(WireError::TrailingBytes(field))
        }
    }
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
    }
    wrap(ATTACH_MAGIC, body)
}

// ---------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------

fn decode_mgmt_body(body: &[u8]) -> Result<DecodedFrame, WireError> {
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

fn decode_attach_body(body: &[u8]) -> Result<DecodedFrame, WireError> {
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
        other => return Err(WireError::UnknownTag(other)),
    };
    Ok(frame)
}

// ---------------------------------------------------------------------
// Splitter
// ---------------------------------------------------------------------

/// Splits an arbitrarily-chunked byte stream from one pipe connection
/// into decoded frames, latching the connection's lane from the first
/// frame's magic and carrying partial data across `feed` calls.
///
/// `feed` never allocates a buffer sized from an untrusted `len`: the
/// accumulated bytes it has actually been given are what it holds, and
/// the [`MAX_BODY_LEN`] check runs the instant the 8-byte header is
/// available — strictly before any attempt to gather (let alone
/// pre-size a buffer for) that many body bytes.
///
/// `feed` returns every frame it decoded THIS call alongside an error if
/// one occurred — frames that completed before a later error in the same
/// call are never dropped. Once an error occurs, this splitter LATCHES
/// failed: its buffer is freed immediately, and every subsequent `feed`
/// call returns the identical error at no cost, without even looking at
/// the bytes it is given. That is the enforced form of "a wire error is
/// connection-fatal" — a caller does not need to remember to stop
/// calling `feed` itself.
#[derive(Debug, Default)]
pub struct FrameSplitter {
    latched: Option<[u8; 4]>,
    buf: Vec<u8>,
    failed: Option<WireError>,
}

/// Once the retained carry drops back below this and the buffer's
/// capacity is still above it, `feed` releases the excess: a single
/// large feed (e.g. concatenated maximum-size frames arriving in one
/// read) must not pin a multi-MiB high-water capacity for the rest of
/// the connection's life. The bound is `2 * MAX_BODY_LEN`, comfortably
/// above the largest possible single-frame carry.
const BUFFER_SHRINK_THRESHOLD: usize = 2 * MAX_BODY_LEN;

impl FrameSplitter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` iff bytes fed so far include some that have not yet formed
    /// a complete frame (or been consumed by one) — a caller with its own
    /// "exactly one reply, nothing more" protocol rule (e.g.
    /// `exchange::VoyageMgmtExchange`) uses this to detect a trailing
    /// partial frame arriving ALONGSIDE a complete one in the same
    /// `feed` call, which `feed`'s own return value (frames + error)
    /// cannot distinguish from "nothing left over" on its own — both
    /// return the same `(frames, None)` shape. Read-only: never resets
    /// or otherwise changes decoder state. `false` after a latched
    /// error (the buffer is freed then, per `feed`'s own doc).
    pub fn has_pending_bytes(&self) -> bool {
        !self.buf.is_empty()
    }

    /// Feeds the next chunk of bytes read from the connection, in order.
    /// Returns every frame that became complete as a result (zero, one,
    /// or more), plus an error if one was encountered while doing so —
    /// frames decoded earlier in THIS call are always included alongside
    /// it, never silently dropped. A chunk cut at any byte boundary,
    /// including one byte at a time, is fully supported.
    ///
    /// After an error, this splitter is failed (see the type doc): every
    /// later call returns `(vec![], Some(<the same error>))` regardless
    /// of what bytes it is given.
    pub fn feed(&mut self, bytes: &[u8]) -> (Vec<DecodedFrame>, Option<WireError>) {
        if let Some(err) = &self.failed {
            return (Vec::new(), Some(err.clone()));
        }

        self.buf.extend_from_slice(bytes);

        let mut out = Vec::new();
        let mut consumed = 0usize;
        let mut error = None;

        loop {
            let available = self.buf.len() - consumed;
            if available < HEADER_LEN {
                break;
            }
            let magic: [u8; 4] = self.buf[consumed..consumed + 4]
                .try_into()
                .expect("checked len");
            if magic != MGMT_MAGIC && magic != ATTACH_MAGIC {
                error = Some(WireError::UnknownMagic(magic));
                break;
            }
            match self.latched {
                None => self.latched = Some(magic),
                Some(latched) if latched != magic => {
                    error = Some(WireError::LaneMismatch {
                        latched,
                        got: magic,
                    });
                    break;
                }
                Some(_) => {}
            }
            let len = u32::from_le_bytes(
                self.buf[consumed + 4..consumed + 8]
                    .try_into()
                    .expect("checked len"),
            );
            if len as usize > MAX_BODY_LEN {
                error = Some(WireError::BodyTooLarge(len));
                break;
            }
            let total = HEADER_LEN + len as usize;
            if available < total {
                break; // Carry: the rest of this body hasn't arrived yet.
            }
            let body_start = consumed + HEADER_LEN;
            let body_end = consumed + total;
            let decode_result = if magic == MGMT_MAGIC {
                decode_mgmt_body(&self.buf[body_start..body_end])
            } else {
                decode_attach_body(&self.buf[body_start..body_end])
            };
            match decode_result {
                Ok(decoded) => {
                    out.push(decoded);
                    consumed += total;
                }
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }

        if let Some(err) = error {
            // Failed-state latch: drop everything (consumed AND
            // unconsumed alike -- the connection is dead either way) and
            // remember the error so every later call is a no-op answer.
            self.buf = Vec::new();
            self.failed = Some(err.clone());
            return (out, Some(err));
        }

        // Compact ONCE per call -- not once per decoded frame, which was
        // quadratic (a `drain` after every frame moves the whole
        // remaining tail each time).
        if consumed > 0 {
            self.buf.drain(0..consumed);
        }

        if self.buf.capacity() > BUFFER_SHRINK_THRESHOLD
            && self.buf.len() < BUFFER_SHRINK_THRESHOLD
        {
            self.buf.shrink_to(BUFFER_SHRINK_THRESHOLD);
        }

        (out, None)
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- goldens -------------------------------------------------------
    //
    // These pin the exact bytes for every frame in both lanes. The mgmt
    // lane's shapes are PERMANENTLY PINNED (ADR 0041): these bytes are the
    // compatibility record a future build must keep reading, not just a
    // snapshot of what this build happens to emit today.

    fn assert_golden(wire: Vec<u8>, expected_hex_bytes: &[u8]) {
        assert_eq!(wire, expected_hex_bytes);
    }

    /// Feeds bytes expected to decode cleanly, panicking with a useful
    /// message if a wire error surfaced instead of the frames.
    fn feed_ok(s: &mut FrameSplitter, bytes: &[u8]) -> Vec<DecodedFrame> {
        let (frames, err) = s.feed(bytes);
        assert_eq!(err, None, "unexpected wire error");
        frames
    }

    /// Feeds bytes expected to produce no frames and exactly one error.
    fn feed_err(s: &mut FrameSplitter, bytes: &[u8]) -> WireError {
        let (frames, err) = s.feed(bytes);
        assert!(frames.is_empty(), "expected no frames before the error, got {frames:?}");
        err.expect("expected a wire error")
    }

    #[test]
    fn golden_mgmt_probe() {
        let wire = encode_mgmt_request(&MgmtRequest::Probe).unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x4d, 0x30, 0x01, 0x00, 0x00, 0x00, 0x01]);
        let mut s = FrameSplitter::new();
        let decoded = feed_ok(&mut s, &wire);
        assert_eq!(decoded, vec![DecodedFrame::MgmtRequest(MgmtRequest::Probe)]);
    }

    #[test]
    fn golden_mgmt_status() {
        let wire = encode_mgmt_request(&MgmtRequest::Status).unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x4d, 0x30, 0x01, 0x00, 0x00, 0x00, 0x02]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::MgmtRequest(MgmtRequest::Status)]
        );
    }

    #[test]
    fn golden_mgmt_shutdown() {
        let wire = encode_mgmt_request(&MgmtRequest::Shutdown {
            reason: "bye".to_string(),
        })
        .unwrap();
        assert_golden(
            wire.clone(),
            &[
                0x53, 0x4f, 0x4d, 0x30, // SOM0
                0x05, 0x00, 0x00, 0x00, // len = 5
                0x03, // tag
                0x03, // reason len = 3
                0x62, 0x79, 0x65, // "bye"
            ],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::MgmtRequest(MgmtRequest::Shutdown {
                reason: "bye".to_string()
            })]
        );
    }

    #[test]
    fn golden_mgmt_probe_ok() {
        let wire = encode_mgmt_reply(&MgmtReply::ProbeOk).unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x4d, 0x30, 0x01, 0x00, 0x00, 0x00, 0x81]);
        let mut s = FrameSplitter::new();
        assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::MgmtReply(MgmtReply::ProbeOk)]);
    }

    #[test]
    fn golden_mgmt_status_ok() {
        let wire = encode_mgmt_reply(&MgmtReply::StatusOk {
            pid: 1,
            created: 2,
            survival: Survival::Normal,
        })
        .unwrap();
        assert_golden(
            wire.clone(),
            &[
                0x53, 0x4f, 0x4d, 0x30, // SOM0
                0x0e, 0x00, 0x00, 0x00, // len = 14
                0x82, // tag
                0x01, 0x00, 0x00, 0x00, // pid = 1
                0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // created = 2
                0x00, // survival = normal
            ],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::MgmtReply(MgmtReply::StatusOk {
                pid: 1,
                created: 2,
                survival: Survival::Normal
            })]
        );
    }

    #[test]
    fn golden_mgmt_status_ok_degraded() {
        let wire = encode_mgmt_reply(&MgmtReply::StatusOk {
            pid: 0xdead_beef,
            created: 0x0102_0304_0506_0708,
            survival: Survival::Degraded,
        })
        .unwrap();
        assert_golden(
            wire.clone(),
            &[
                0x53, 0x4f, 0x4d, 0x30, 0x0e, 0x00, 0x00, 0x00, 0x82, 0xef, 0xbe, 0xad, 0xde,
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x01,
            ],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::MgmtReply(MgmtReply::StatusOk {
                pid: 0xdead_beef,
                created: 0x0102_0304_0506_0708,
                survival: Survival::Degraded
            })]
        );
    }

    #[test]
    fn golden_mgmt_shutdown_ok() {
        let wire = encode_mgmt_reply(&MgmtReply::ShutdownOk).unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x4d, 0x30, 0x01, 0x00, 0x00, 0x00, 0x83]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::MgmtReply(MgmtReply::ShutdownOk)]
        );
    }

    #[test]
    fn golden_attach_hello() {
        let wire = encode_attach_client(&AttachClient::Hello { proto: 1 }).unwrap();
        assert_golden(
            wire.clone(),
            &[0x53, 0x4f, 0x41, 0x30, 0x05, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachClient(AttachClient::Hello { proto: 1 })]
        );
    }

    #[test]
    fn golden_attach_attach() {
        let wire = encode_attach_client(&AttachClient::Attach {
            controller_id: "c1".to_string(),
        })
        .unwrap();
        assert_golden(
            wire.clone(),
            &[0x53, 0x4f, 0x41, 0x30, 0x04, 0x00, 0x00, 0x00, 0x02, 0x02, 0x63, 0x31],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachClient(AttachClient::Attach {
                controller_id: "c1".to_string()
            })]
        );
    }

    #[test]
    fn golden_attach_take() {
        let wire = encode_attach_client(&AttachClient::Take {
            controller_id: "c1".to_string(),
        })
        .unwrap();
        assert_golden(
            wire.clone(),
            &[0x53, 0x4f, 0x41, 0x30, 0x04, 0x00, 0x00, 0x00, 0x03, 0x02, 0x63, 0x31],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachClient(AttachClient::Take {
                controller_id: "c1".to_string()
            })]
        );
    }

    #[test]
    fn golden_attach_input() {
        let idem_key: [u8; 16] = [
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
            0x1e, 0x1f,
        ];
        let wire = encode_attach_client(&AttachClient::Input {
            controller_id: "c1".to_string(),
            take_epoch: 7,
            idem_key,
            payload: b"hi".to_vec(),
        })
        .unwrap();
        assert_golden(
            wire.clone(),
            &[
                0x53, 0x4f, 0x41, 0x30, // SOA0
                0x20, 0x00, 0x00, 0x00, // len = 32
                0x04, // tag
                0x02, 0x63, 0x31, // controller_id "c1"
                0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // take_epoch = 7
                0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
                0x1d, 0x1e, 0x1f, // idem_key
                0x02, 0x00, // payload len = 2
                0x68, 0x69, // "hi"
            ],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachClient(AttachClient::Input {
                controller_id: "c1".to_string(),
                take_epoch: 7,
                idem_key,
                payload: b"hi".to_vec(),
            })]
        );
    }

    #[test]
    fn golden_attach_resize() {
        let wire = encode_attach_client(&AttachClient::Resize { cols: 80, rows: 24 }).unwrap();
        assert_golden(
            wire.clone(),
            &[0x53, 0x4f, 0x41, 0x30, 0x05, 0x00, 0x00, 0x00, 0x05, 0x50, 0x00, 0x18, 0x00],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachClient(AttachClient::Resize { cols: 80, rows: 24 })]
        );
    }

    #[test]
    fn golden_keepalive() {
        // One direction-neutral frame, not one per side (see the module
        // doc's tag-table note).
        let wire = encode_keepalive(42);
        assert_golden(
            wire.clone(),
            &[
                0x53, 0x4f, 0x41, 0x30, 0x09, 0x00, 0x00, 0x00, 0x06, 0x2a, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::Keepalive { nonce: 42 }]);
    }

    #[test]
    fn keepalive_is_byte_identical_whichever_side_sends_it() {
        // The ADR pins ONE echo frame: the server originates it, and the
        // client's reply must be the identical bytes bounced back. There
        // is exactly one encoder, so "the server's frame" and "the
        // client's echo" are, by construction, the same bytes.
        let server_sent = encode_keepalive(0xdead_beef);
        let client_echo = encode_keepalive(0xdead_beef);
        assert_eq!(server_sent, client_echo);

        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &server_sent),
            vec![DecodedFrame::Keepalive { nonce: 0xdead_beef }]
        );
    }

    #[test]
    fn golden_attach_hello_ok() {
        let wire = encode_attach_server(&AttachServer::HelloOk { proto: 1 }).unwrap();
        assert_golden(
            wire.clone(),
            &[0x53, 0x4f, 0x41, 0x30, 0x05, 0x00, 0x00, 0x00, 0x81, 0x01, 0x00, 0x00, 0x00],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::HelloOk { proto: 1 })]
        );
    }

    #[test]
    fn golden_attach_hello_refused() {
        let wire = encode_attach_server(&AttachServer::HelloRefused { supported: 1 }).unwrap();
        assert_golden(
            wire.clone(),
            &[0x53, 0x4f, 0x41, 0x30, 0x05, 0x00, 0x00, 0x00, 0x82, 0x01, 0x00, 0x00, 0x00],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::HelloRefused { supported: 1 })]
        );
    }

    #[test]
    fn golden_attach_checkpoint_chunk() {
        let wire = encode_attach_server(&AttachServer::CheckpointChunk {
            last: true,
            bytes: b"AB".to_vec(),
        })
        .unwrap();
        assert_golden(
            wire.clone(),
            &[0x53, 0x4f, 0x41, 0x30, 0x04, 0x00, 0x00, 0x00, 0x83, 0x01, 0x41, 0x42],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::CheckpointChunk {
                last: true,
                bytes: b"AB".to_vec()
            })]
        );
    }

    #[test]
    fn golden_attach_refused() {
        let wire = encode_attach_server(&AttachServer::AttachRefused {
            reason: AttachRefusedReason::GroundTimeout,
        })
        .unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x84, 0x00]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::AttachRefused {
                reason: AttachRefusedReason::GroundTimeout
            })]
        );
    }

    #[test]
    fn golden_attach_output() {
        let wire = encode_attach_server(&AttachServer::Output {
            bytes: b"hi".to_vec(),
        })
        .unwrap();
        assert_golden(
            wire.clone(),
            &[0x53, 0x4f, 0x41, 0x30, 0x03, 0x00, 0x00, 0x00, 0x85, 0x68, 0x69],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::Output {
                bytes: b"hi".to_vec()
            })]
        );
    }

    #[test]
    fn golden_attach_take_ok() {
        let wire = encode_attach_server(&AttachServer::TakeOk { take_epoch: 9 }).unwrap();
        assert_golden(
            wire.clone(),
            &[
                0x53, 0x4f, 0x41, 0x30, 0x09, 0x00, 0x00, 0x00, 0x86, 0x09, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::TakeOk { take_epoch: 9 })]
        );
    }

    #[test]
    fn golden_take_refused() {
        let wire = encode_attach_server(&AttachServer::TakeRefused {
            reason: TakeRefusedReason::NotAttached,
        })
        .unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x87, 0x00]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::TakeRefused {
                reason: TakeRefusedReason::NotAttached
            })]
        );
    }

    #[test]
    fn golden_input_recorded() {
        let wire = encode_attach_server(&AttachServer::InputRecorded).unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x01, 0x00, 0x00, 0x00, 0x88]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::InputRecorded)]
        );
    }

    #[test]
    fn golden_input_refused_stale() {
        let wire = encode_attach_server(&AttachServer::InputRefusedStale).unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x01, 0x00, 0x00, 0x00, 0x89]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::InputRefusedStale)]
        );
    }

    #[test]
    fn golden_input_delivery_unknown() {
        let wire = encode_attach_server(&AttachServer::InputDeliveryUnknown).unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x01, 0x00, 0x00, 0x00, 0x8a]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::InputDeliveryUnknown)]
        );
    }

    #[test]
    fn golden_resize_ok() {
        let wire = encode_attach_server(&AttachServer::ResizeOk).unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x01, 0x00, 0x00, 0x00, 0x8b]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::ResizeOk)]
        );
    }

    #[test]
    fn golden_resize_refused() {
        let wire = encode_attach_server(&AttachServer::ResizeRefused {
            reason: ResizeRefusedReason::OutOfBudget,
        })
        .unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x8c, 0x00]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::ResizeRefused {
                reason: ResizeRefusedReason::OutOfBudget
            })]
        );
    }

    // ---- lane binding ---------------------------------------------------

    #[test]
    fn attach_then_mgmt_is_a_lane_mismatch() {
        let attach = encode_attach_client(&AttachClient::Hello { proto: 1 }).unwrap();
        let mgmt = encode_mgmt_request(&MgmtRequest::Probe).unwrap();
        let mut s = FrameSplitter::new();
        feed_ok(&mut s, &attach);
        let err = feed_err(&mut s, &mgmt);
        assert_eq!(
            err,
            WireError::LaneMismatch {
                latched: ATTACH_MAGIC,
                got: MGMT_MAGIC
            }
        );
    }

    #[test]
    fn mgmt_then_attach_is_a_lane_mismatch() {
        let mgmt = encode_mgmt_request(&MgmtRequest::Probe).unwrap();
        let attach = encode_attach_client(&AttachClient::Hello { proto: 1 }).unwrap();
        let mut s = FrameSplitter::new();
        feed_ok(&mut s, &mgmt);
        let err = feed_err(&mut s, &attach);
        assert_eq!(
            err,
            WireError::LaneMismatch {
                latched: MGMT_MAGIC,
                got: ATTACH_MAGIC
            }
        );
    }

    #[test]
    fn unknown_magic_errors() {
        let mut bogus = Vec::new();
        bogus.extend_from_slice(b"XXXX");
        push_u32(&mut bogus, 0);
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &bogus), WireError::UnknownMagic(*b"XXXX"));
    }

    // ---- cap --------------------------------------------------------------

    #[test]
    fn header_announcing_over_cap_len_errors_without_the_body() {
        let mut header_only = Vec::new();
        header_only.extend_from_slice(&MGMT_MAGIC);
        push_u32(&mut header_only, (MAX_BODY_LEN as u32) + 1);
        // No body bytes at all follow -- if this errored by trying to
        // gather that many bytes first it would just carry (return Ok
        // with no frames) instead of erroring.
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_err(&mut s, &header_only),
            WireError::BodyTooLarge((MAX_BODY_LEN as u32) + 1)
        );
    }

    #[test]
    fn header_at_exactly_the_cap_is_not_an_error() {
        let mut header_only = Vec::new();
        header_only.extend_from_slice(&MGMT_MAGIC);
        push_u32(&mut header_only, MAX_BODY_LEN as u32);
        let mut s = FrameSplitter::new();
        // Not enough body bytes yet -- carry, not an error.
        assert_eq!(feed_ok(&mut s, &header_only), Vec::new());
    }

    // ---- bounds -------------------------------------------------------

    #[test]
    fn controller_id_over_128_bytes_refused_at_encode() {
        let controller_id = "a".repeat(129);
        let err = encode_attach_client(&AttachClient::Attach { controller_id }).unwrap_err();
        assert_eq!(
            err,
            WireError::FieldTooLarge {
                field: "controller_id",
                len: 129,
                max: 128
            }
        );
    }

    #[test]
    fn controller_id_over_128_bytes_refused_at_decode() {
        // Hand-built: a 129-byte controller_id, which no `encode_*`
        // helper here will ever produce, but a hostile or buggy peer
        // could send.
        let mut body = vec![TAG_ATTACH_REQ_ATTACH, 129u8];
        body.extend(std::iter::repeat_n(b'a', 129));
        let wire = wrap(ATTACH_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_err(&mut s, &wire),
            WireError::FieldTooLarge {
                field: "controller_id",
                len: 129,
                max: 128
            }
        );
    }

    #[test]
    fn input_payload_over_8192_bytes_refused_at_encode() {
        let err = encode_attach_client(&AttachClient::Input {
            controller_id: "c".to_string(),
            take_epoch: 0,
            idem_key: [0; 16],
            payload: vec![0u8; 8193],
        })
        .unwrap_err();
        assert_eq!(
            err,
            WireError::FieldTooLarge {
                field: "input.payload",
                len: 8193,
                max: 8192
            }
        );
    }

    #[test]
    fn input_payload_over_8192_bytes_refused_at_decode() {
        let mut body = vec![TAG_ATTACH_REQ_INPUT, 1u8, b'c'];
        push_u64(&mut body, 0);
        body.extend_from_slice(&[0u8; 16]);
        push_u16(&mut body, 8193);
        body.extend(std::iter::repeat_n(0u8, 8193));
        let wire = wrap(ATTACH_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_err(&mut s, &wire),
            WireError::FieldTooLarge {
                field: "input.payload",
                len: 8193,
                max: 8192
            }
        );
    }

    #[test]
    fn shutdown_reason_over_128_bytes_refused_at_encode() {
        let err = encode_mgmt_request(&MgmtRequest::Shutdown {
            reason: "x".repeat(129),
        })
        .unwrap_err();
        assert_eq!(
            err,
            WireError::FieldTooLarge {
                field: "shutdown.reason",
                len: 129,
                max: 128
            }
        );
    }

    #[test]
    fn shutdown_reason_over_128_bytes_refused_at_decode() {
        let mut body = vec![TAG_MGMT_REQ_SHUTDOWN, 129u8];
        body.extend(std::iter::repeat_n(b'x', 129));
        let wire = wrap(MGMT_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_err(&mut s, &wire),
            WireError::FieldTooLarge {
                field: "shutdown.reason",
                len: 129,
                max: 128
            }
        );
    }

    #[test]
    fn non_utf8_controller_id_refused_at_decode() {
        let body = vec![TAG_ATTACH_REQ_ATTACH, 1u8, 0xff];
        let wire = wrap(ATTACH_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_err(&mut s, &wire),
            WireError::InvalidUtf8("controller_id")
        );
    }

    #[test]
    fn non_utf8_shutdown_reason_refused_at_decode() {
        let body = vec![TAG_MGMT_REQ_SHUTDOWN, 1u8, 0xff];
        let wire = wrap(MGMT_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_err(&mut s, &wire),
            WireError::InvalidUtf8("shutdown.reason")
        );
    }

    // ---- unknown tag / trailing bytes / unknown reason -----------------

    #[test]
    fn unknown_mgmt_tag_errors() {
        let wire = wrap(MGMT_MAGIC, vec![0x99]).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::UnknownTag(0x99));
    }

    #[test]
    fn unknown_attach_tag_errors() {
        let wire = wrap(ATTACH_MAGIC, vec![0x50]).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::UnknownTag(0x50));
    }

    #[test]
    fn trailing_bytes_after_a_fixed_body_errors() {
        let wire = wrap(MGMT_MAGIC, vec![TAG_MGMT_REQ_PROBE, 0xff]).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::TrailingBytes("probe"));
    }

    #[test]
    fn unknown_survival_value_errors() {
        let mut body = vec![TAG_MGMT_REP_STATUS_OK];
        push_u32(&mut body, 1);
        push_u64(&mut body, 1);
        body.push(2); // neither 0 (normal) nor 1 (degraded)
        let wire = wrap(MGMT_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_err(&mut s, &wire),
            WireError::UnknownEnumValue {
                field: "status_ok.survival",
                value: 2
            }
        );
    }

    #[test]
    fn unknown_attach_refused_reason_errors() {
        let wire = wrap(ATTACH_MAGIC, vec![TAG_ATTACH_REP_ATTACH_REFUSED, 7]).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_err(&mut s, &wire),
            WireError::UnknownEnumValue {
                field: "attach_refused.reason",
                value: 7
            }
        );
    }

    #[test]
    fn checkpoint_chunk_last_flag_must_be_0_or_1() {
        let wire = wrap(ATTACH_MAGIC, vec![TAG_ATTACH_REP_CHECKPOINT_CHUNK, 5]).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_err(&mut s, &wire),
            WireError::UnknownEnumValue {
                field: "checkpoint_chunk.last",
                value: 5
            }
        );
    }

    // ---- hello negotiation ---------------------------------------------

    #[test]
    fn negotiate_accepts_v1() {
        assert_eq!(negotiate(1), Negotiated::Accepted(1));
    }

    #[test]
    fn negotiate_refuses_anything_else() {
        assert_eq!(negotiate(2), Negotiated::Refused { supported: 1 });
        assert_eq!(negotiate(0), Negotiated::Refused { supported: 1 });
    }

    // ---- chunk arithmetic ------------------------------------------------

    #[test]
    fn greedy_chunking_of_the_max_checkpoint_uses_the_computed_count() {
        assert_eq!(MAX_CHECKPOINT_CHUNK_PAYLOAD, 1_048_574);
        assert_eq!(CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD, 9);

        let mut remaining = MAX_CHECKPOINT_LEN;
        let mut splitter = FrameSplitter::new();
        let mut recovered_len = 0usize;
        let mut chunk_count = 0usize;
        while remaining > 0 {
            let take = remaining.min(MAX_CHECKPOINT_CHUNK_PAYLOAD);
            let is_last = take == remaining;
            let bytes = vec![0xabu8; take];
            let frame = AttachServer::CheckpointChunk {
                last: is_last,
                bytes,
            };
            let wire = encode_attach_server(&frame).expect("within the frame cap");
            assert!(
                wire.len() <= HEADER_LEN + MAX_BODY_LEN,
                "checkpoint_chunk frame exceeds the outer 1 MiB cap"
            );
            let decoded = feed_ok(&mut splitter, &wire);
            assert_eq!(decoded.len(), 1);
            match &decoded[0] {
                DecodedFrame::AttachServer(AttachServer::CheckpointChunk { last, bytes }) => {
                    assert_eq!(*last, is_last);
                    recovered_len += bytes.len();
                }
                other => panic!("expected a checkpoint_chunk, got {other:?}"),
            }
            remaining -= take;
            chunk_count += 1;
        }
        assert_eq!(chunk_count, CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD);
        assert_eq!(recovered_len, MAX_CHECKPOINT_LEN);
    }

    #[test]
    fn more_chunks_than_the_max_payload_count_decode_fine() {
        // CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD is what a GREEDY encoder
        // produces at the largest possible checkpoint -- it is NOT a
        // protocol ceiling. A sender using smaller chunks may legally
        // emit more of them; the wire never counts or caps
        // `checkpoint_chunk` frames, only bytes-per-frame.
        let mut splitter = FrameSplitter::new();
        let total_chunks = CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD + 5;
        for i in 0..total_chunks {
            let wire = encode_attach_server(&AttachServer::CheckpointChunk {
                last: i + 1 == total_chunks,
                bytes: vec![0x11u8; 4],
            })
            .unwrap();
            assert_eq!(feed_ok(&mut splitter, &wire).len(), 1);
        }
    }

    // ---- should-fix 5: golden/edge completeness --------------------------

    #[test]
    fn golden_attach_refused_subscriber_cap() {
        let wire = encode_attach_server(&AttachServer::AttachRefused {
            reason: AttachRefusedReason::SubscriberCap,
        })
        .unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x84, 0x01]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::AttachRefused {
                reason: AttachRefusedReason::SubscriberCap
            })]
        );
    }

    #[test]
    fn golden_take_refused_checkpoint_in_flight() {
        let wire = encode_attach_server(&AttachServer::TakeRefused {
            reason: TakeRefusedReason::CheckpointInFlight,
        })
        .unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x87, 0x01]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::TakeRefused {
                reason: TakeRefusedReason::CheckpointInFlight
            })]
        );
    }

    #[test]
    fn golden_resize_refused_not_driver() {
        let wire = encode_attach_server(&AttachServer::ResizeRefused {
            reason: ResizeRefusedReason::NotDriver,
        })
        .unwrap();
        assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x8c, 0x01]);
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::ResizeRefused {
                reason: ResizeRefusedReason::NotDriver
            })]
        );
    }

    #[test]
    fn golden_attach_checkpoint_chunk_not_last() {
        let wire = encode_attach_server(&AttachServer::CheckpointChunk {
            last: false,
            bytes: b"AB".to_vec(),
        })
        .unwrap();
        assert_golden(
            wire.clone(),
            &[0x53, 0x4f, 0x41, 0x30, 0x04, 0x00, 0x00, 0x00, 0x83, 0x00, 0x41, 0x42],
        );
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::CheckpointChunk {
                last: false,
                bytes: b"AB".to_vec()
            })]
        );
    }

    #[test]
    fn controller_id_at_exactly_128_bytes_is_legal_both_ways() {
        let controller_id = "a".repeat(128);
        let wire = encode_attach_client(&AttachClient::Attach {
            controller_id: controller_id.clone(),
        })
        .unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachClient(AttachClient::Attach { controller_id })]
        );
    }

    #[test]
    fn shutdown_reason_at_exactly_128_bytes_is_legal_both_ways() {
        let reason = "x".repeat(128);
        let wire = encode_mgmt_request(&MgmtRequest::Shutdown {
            reason: reason.clone(),
        })
        .unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::MgmtRequest(MgmtRequest::Shutdown { reason })]
        );
    }

    #[test]
    fn input_payload_at_exactly_8192_bytes_is_legal_both_ways() {
        let payload = vec![0xabu8; 8192];
        let wire = encode_attach_client(&AttachClient::Input {
            controller_id: "c".to_string(),
            take_epoch: 1,
            idem_key: [7u8; 16],
            payload: payload.clone(),
        })
        .unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachClient(AttachClient::Input {
                controller_id: "c".to_string(),
                take_epoch: 1,
                idem_key: [7u8; 16],
                payload,
            })]
        );
    }

    #[test]
    fn status_ok_body_one_byte_short_errors() {
        let mut body = vec![TAG_MGMT_REP_STATUS_OK];
        push_u32(&mut body, 1);
        push_u64(&mut body, 2);
        // Missing the trailing survival byte: 13 bytes total, not 14.
        let wire = wrap(MGMT_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::Malformed("status_ok.survival"));
    }

    #[test]
    fn status_ok_body_one_byte_long_errors() {
        let mut body = vec![TAG_MGMT_REP_STATUS_OK];
        push_u32(&mut body, 1);
        push_u64(&mut body, 2);
        body.push(0); // survival = normal
        body.push(0xff); // one byte past the defined shape
        let wire = wrap(MGMT_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::TrailingBytes("status_ok"));
    }

    #[test]
    fn take_ok_body_one_byte_short_errors() {
        let mut body = vec![TAG_ATTACH_REP_TAKE_OK];
        body.extend_from_slice(&9u64.to_le_bytes()[..7]); // 7 of take_epoch's 8 bytes
        let wire = wrap(ATTACH_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::Malformed("take_ok.take_epoch"));
    }

    #[test]
    fn take_ok_body_one_byte_long_errors() {
        let mut body = vec![TAG_ATTACH_REP_TAKE_OK];
        push_u64(&mut body, 9);
        body.push(0xff);
        let wire = wrap(ATTACH_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::TrailingBytes("take_ok"));
    }

    #[test]
    fn resize_body_one_byte_short_errors() {
        let mut body = vec![TAG_ATTACH_REQ_RESIZE];
        push_u16(&mut body, 80);
        body.push(24); // only 1 of rows' 2 bytes
        let wire = wrap(ATTACH_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::Malformed("resize.rows"));
    }

    #[test]
    fn resize_body_one_byte_long_errors() {
        let mut body = vec![TAG_ATTACH_REQ_RESIZE];
        push_u16(&mut body, 80);
        push_u16(&mut body, 24);
        body.push(0xff);
        let wire = wrap(ATTACH_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::TrailingBytes("resize"));
    }

    #[test]
    fn empty_controller_id_rejected_at_encode() {
        let err = encode_attach_client(&AttachClient::Attach {
            controller_id: String::new(),
        })
        .unwrap_err();
        assert_eq!(err, WireError::FieldEmpty("controller_id"));
    }

    #[test]
    fn empty_controller_id_rejected_at_decode() {
        let body = vec![TAG_ATTACH_REQ_ATTACH, 0u8]; // len = 0
        let wire = wrap(ATTACH_MAGIC, body).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_err(&mut s, &wire), WireError::FieldEmpty("controller_id"));
    }

    #[test]
    fn empty_input_payload_is_legal() {
        // Deliberate, unlike controller_id: a payload field's "nothing
        // this round" is a real, meaningful state, not a malformed
        // identity (see "Field minimums" in the module doc).
        let wire = encode_attach_client(&AttachClient::Input {
            controller_id: "c".to_string(),
            take_epoch: 0,
            idem_key: [0; 16],
            payload: Vec::new(),
        })
        .unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachClient(AttachClient::Input {
                controller_id: "c".to_string(),
                take_epoch: 0,
                idem_key: [0; 16],
                payload: Vec::new(),
            })]
        );
    }

    #[test]
    fn empty_output_bytes_is_legal() {
        // Deliberate, same reasoning as the input payload above.
        let wire = encode_attach_server(&AttachServer::Output { bytes: Vec::new() }).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::Output { bytes: Vec::new() })]
        );
    }

    #[test]
    fn empty_non_final_checkpoint_chunk_is_legal() {
        // Deliberate: a zero-byte non-final chunk is unusual but not
        // malformed -- the wire does not encode "why" a writer produced
        // one, and total-byte bounding is the consumer's job (see
        // "Checkpoint chunk arithmetic" in the module doc).
        let wire = encode_attach_server(&AttachServer::CheckpointChunk {
            last: false,
            bytes: Vec::new(),
        })
        .unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::AttachServer(AttachServer::CheckpointChunk {
                last: false,
                bytes: Vec::new()
            })]
        );
    }

    // ---- should-fix 3: error semantics + the failed-state latch --------

    #[test]
    fn feeding_after_failure_ignores_bytes_frees_memory_and_repeats_the_error() {
        let bad = wrap(MGMT_MAGIC, vec![0x99]).unwrap(); // unknown tag
        let mut s = FrameSplitter::new();
        let first_err = feed_err(&mut s, &bad);
        assert_eq!(first_err, WireError::UnknownTag(0x99));
        assert!(s.buf.is_empty(), "buffer must be dropped once failed");

        // A large blob of unrelated garbage must not grow memory or
        // change the answer.
        let garbage = vec![0xffu8; 5 * MAX_BODY_LEN];
        let (frames, err) = s.feed(&garbage);
        assert!(frames.is_empty());
        assert_eq!(err, Some(first_err.clone()));
        assert!(s.buf.is_empty(), "a failed splitter must never retain fed bytes");

        // And again -- the answer must not change call to call.
        let (frames2, err2) = s.feed(&[1, 2, 3]);
        assert!(frames2.is_empty());
        assert_eq!(err2, Some(first_err));
    }

    #[test]
    fn frames_decoded_before_a_same_call_error_are_still_returned() {
        // Two valid probe frames followed by one with an unknown tag, all
        // fed in a SINGLE `feed` call: the first two must come back
        // alongside the error, not be dropped by it.
        let mut bytes = Vec::new();
        bytes.extend(encode_mgmt_request(&MgmtRequest::Probe).unwrap());
        bytes.extend(encode_mgmt_request(&MgmtRequest::Status).unwrap());
        bytes.extend(wrap(MGMT_MAGIC, vec![0x99]).unwrap());

        let mut s = FrameSplitter::new();
        let (frames, err) = s.feed(&bytes);
        assert_eq!(
            frames,
            vec![
                DecodedFrame::MgmtRequest(MgmtRequest::Probe),
                DecodedFrame::MgmtRequest(MgmtRequest::Status),
            ]
        );
        assert_eq!(err, Some(WireError::UnknownTag(0x99)));
    }

    /// The pinned literal above must never drift from the fork's proven
    /// bound. The fork is a windows-gated dependency of this crate, so the
    /// cross-check runs on the windows CI legs — which is where the number
    /// is ever consumed.
    #[cfg(windows)]
    #[test]
    fn pinned_checkpoint_len_matches_the_fork() {
        assert_eq!(MAX_CHECKPOINT_LEN, vt100_ctt::MAX_CHECKPOINT_LEN);
    }
}
