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
//! | SOA0 | S→C | `0x8d` | `pen_snapshot`           | v3 only: `holder`: presence u8 + (len u8 + UTF-8, ≤128 B) if present, `take_epoch`:u64-LE |
//! | SOA0 | S→C | `0x8e` | `pen_changed`            | v3 only: same shape as `pen_snapshot` |
//! | SOA0 | S→C | `0x8f` | `geometry`               | v3 only: `cols`:u16-LE, `rows`:u16-LE |
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
//! ADR 0041 maximum 512×256 geometry, PLUS a full 200-row scrollback ring
//! on the normal grid — `rust/vt100/src/checkpoint.rs`'s
//! `MAX_CHECKPOINT_LEN`) is a PROVEN 12,030,729 bytes. This module pins
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
//! worst-case checkpoint — 12 — but it is NOT a protocol maximum: nothing
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

/// ADR 0041 step 6 U2: the supervisor lane's magic (`SOSV`) — a THIRD
/// lane, distinct from both mgmt (permanently pinned) and attach
/// (versioned via `hello`, negotiated). The supervisor lane carries its
/// own build identity per connection instead (see [`SupervisorRequest::Hello`]):
/// "file replacement is not process replacement," so a mismatched pair is
/// refused rather than negotiated down.
pub const SUPERVISOR_MAGIC: [u8; 4] = *b"SOSV";

/// Bytes in the outer header (`magic` + `len`), ahead of the body.
const HEADER_LEN: usize = 4 + 4;

/// The body-length cap (1 MiB), enforced before the splitter looks for
/// that many body bytes.
pub const MAX_BODY_LEN: usize = 1_048_576;

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
/// ADR 0046 decision 3, lane B3b1: v3-only, owner-emitted pen/geometry
/// events (see `AttachServer::PenSnapshot`'s own doc for the three new
/// shapes these tags carry).
const TAG_ATTACH_REP_PEN_SNAPSHOT: u8 = 0x8d;
const TAG_ATTACH_REP_PEN_CHANGED: u8 = 0x8e;
const TAG_ATTACH_REP_GEOMETRY: u8 = 0x8f;

const TAG_SV_REQ_HELLO: u8 = 0x01;
const TAG_SV_REQ_COMMAND: u8 = 0x02;
const TAG_SV_REQ_STATUS: u8 = 0x03;
const TAG_SV_REQ_QUERY: u8 = 0x04;
const TAG_SV_REP_HELLO_OK: u8 = 0x81;
const TAG_SV_REP_REFUSED: u8 = 0x82;
const TAG_SV_REP_OPERATION: u8 = 0x83;
const TAG_SV_REP_STATUS_OK: u8 = 0x84;

/// `command`'s nested `op` discriminator.
const TAG_SV_OP_END_RUN: u8 = 0x01;
const TAG_SV_OP_RESET: u8 = 0x02;
const TAG_SV_OP_STOP: u8 = 0x03;

/// `SupervisorOperationState`'s own discriminator — carried inside
/// `TAG_SV_REP_OPERATION`, since both `command` and `query` reply with
/// the identical vocabulary (ADR 0041: "`query` returns accepted |
/// in_progress | record_closed | record_verified | reset_done | stopping
/// | failed | refused | unknown_operation").
const TAG_SV_OPSTATE_ACCEPTED: u8 = 0x01;
// 0x02 ("in_progress") is retired -- see `SupervisorOperationState`'s own
// doc; never emitted, so decoding it now correctly falls into the
// generic `UnknownTag` arm rather than being reserved for a value this
// authority can never produce.
const TAG_SV_OPSTATE_RECORD_CLOSED: u8 = 0x03;
const TAG_SV_OPSTATE_RECORD_VERIFIED: u8 = 0x04;
const TAG_SV_OPSTATE_RESET_DONE: u8 = 0x05;
const TAG_SV_OPSTATE_STOPPING: u8 = 0x06;
const TAG_SV_OPSTATE_FAILED: u8 = 0x07;
const TAG_SV_OPSTATE_REFUSED: u8 = 0x08;
const TAG_SV_OPSTATE_UNKNOWN_OPERATION: u8 = 0x09;

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
    /// A supervisor-lane `operation_id` was well-formed as a bounded
    /// string but not a legal id: it must match `[A-Za-z0-9._-]{1,64}`
    /// and be neither `.` nor `..` (ADR 0041 step 6 U2, Codex review
    /// finding 6). `operation_id` is interpolated directly into a
    /// Windows filesystem path (`<state_dir>/supervisor-journal/<id>.*`)
    /// with no further sanitization downstream, so this is where the
    /// journal is protected from path traversal, absolute paths, and
    /// separator/reserved-name confusion — the journal itself must never
    /// see an unvalidated id.
    #[error("field {field} is not a legal operation_id: {value:?}")]
    InvalidOperationId { field: &'static str, value: String },
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
    SupervisorRequest(SupervisorRequest),
    SupervisorReply(SupervisorReply),
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

/// Shared by `AttachServer::PenSnapshot`/`PenChanged`: an optional
/// holder string, encoded exactly like `SupervisorOp::Reset`'s optional
/// `voyage` field — a presence flag byte, then the bounded string only
/// if present. Reuses [`MAX_CONTROLLER_ID_LEN`]: a `holder` IS a
/// `controller_id`, just observed from the other side of the wire.
fn push_holder(body: &mut Vec<u8>, holder: &Option<String>) -> Result<(), WireError> {
    body.push(holder.is_some() as u8);
    if let Some(holder) = holder {
        push_bounded_string(body, holder, MAX_CONTROLLER_ID_LEN, "holder", true)?;
    }
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

    /// The decode side of [`push_holder`]: a presence flag, then the
    /// bounded string only if present.
    fn holder(&mut self, field: &'static str) -> Result<Option<String>, WireError> {
        let present = self.bool_flag("holder.present")?;
        if present {
            Ok(Some(self.bounded_string(MAX_CONTROLLER_ID_LEN, field, true)?))
        } else {
            Ok(None)
        }
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
            if magic != MGMT_MAGIC && magic != ATTACH_MAGIC && magic != SUPERVISOR_MAGIC {
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
            } else if magic == ATTACH_MAGIC {
                decode_attach_body(&self.buf[body_start..body_end])
            } else {
                decode_supervisor_body(&self.buf[body_start..body_end])
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


mod attach;
mod mgmt;
mod supervisor;

pub use attach::*;
pub use mgmt::*;
pub use supervisor::*;

#[cfg(test)]
mod bounds_tests;
#[cfg(test)]
mod framing_tests;
#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod support_tests;
#[cfg(test)]
mod supervisor_tests;
