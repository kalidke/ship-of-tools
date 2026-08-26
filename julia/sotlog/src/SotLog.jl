"""
    SotLog

Cross-language golden READER for the Ship's Log segment format (ADR 0039:
"Voyage frame codec and segment format"). This package reads `.sotseg`
segment files written by the Rust `sot-log` crate (`rust/log/`) and is the
Julia half of the ADR's merge gate: "cross-language golden fixtures (Rust
writes, Julia reads — the fixtures are conformance tests for this ADR, not
its substitute)".

READER ONLY — no writer. See `docs/adr/0039-voyage-frame-codec-and-segment-format.md`
for the normative spec; this module implements exactly that document, byte
for byte, against `rust/log/src/record.rs` and `rust/log/src/segment.rs` as
the reference implementation.

Scope note: this reader always treats its input as a **sealed** `.sotseg`
file (the ADR's "in `.sotseg` every defect is loud, and a valid seal must
end exactly at EOF" rule) — it does not implement the writer-side recovery
states (`.open`/`.recovering`/`.recovering-out`) or the ADR's tail-tear
promotion nuance for those states. Any provable tear (truncated prelude or
short body) is surfaced directly as a [`TornTailError`](@ref); any byte
appended after a segment's seal record is rejected as `record after seal`
without attempting to parse it — a strict superset of the ADR's "a post-seal
suffix is loud in every state".
"""
module SotLog

using CRC32c: crc32c
using JSON3
using SHA: SHA256_CTX, update!, digest!

export Segment, Seq, Digest, HeaderBody, SealBody
export read_segment, verify_seal
export SotLogError, CorruptRecordError, TornTailError, SchemaError, SealMismatchError

# ---------------------------------------------------------------------------
# Errors — fail closed, one concrete exception type per failure category.
# ---------------------------------------------------------------------------

"Abstract supertype of every error this package throws."
abstract type SotLogError <: Exception end

"""
    CorruptRecordError(offset, reason)

A **loud** defect (ADR 0039 tail rule, case (c)): a complete-but-invalid
record prelude/body (bad magic, unknown wrapper version/record kind/codec,
nonzero reserved byte, a CRC mismatch), or a structural ordering violation
(a frame or seal before the header, a second header, or any byte appended
after a segment's seal record). `offset` is the 1-based byte offset into the
segment where the defect was found.
"""
struct CorruptRecordError <: SotLogError
    offset::Int
    reason::String
end

"""
    TornTailError(offset, reason)

A **provably incomplete final record** (ADR 0039 tail rule, cases (a)/(b)):
fewer than 18 bytes remain for the prelude, or a valid prelude names a body
longer than what remains in the buffer. Distinct from [`CorruptRecordError`](@ref)
because a tear is a truncation, not bit-rot or a format violation.
"""
struct TornTailError <: SotLogError
    offset::Int
    reason::String
end

"""
    SchemaError(reason)

A record's body parsed as valid JSON but violates an ADR 0039 schema rule
this reader checks: a missing `seq.epoch`/`seq.n`, `seq.n < 1`, or a frame
`class` outside the eight closed-enum values.
"""
struct SchemaError <: SotLogError
    reason::String
end

"""
    SealMismatchError(reason)

[`verify_seal`](@ref) failed: the recomputed seal digest disagrees with the
stored one, or the seal's `frame_count`/`first_seq`/`last_seq` metadata
disagrees with the parsed frames.
"""
struct SealMismatchError <: SotLogError
    reason::String
end

Base.showerror(io::IO, e::CorruptRecordError) =
    print(io, "SotLog corrupt record at offset ", e.offset, ": ", e.reason)
Base.showerror(io::IO, e::TornTailError) =
    print(io, "SotLog torn tail at offset ", e.offset, ": ", e.reason)
Base.showerror(io::IO, e::SchemaError) =
    print(io, "SotLog schema violation: ", e.reason)
Base.showerror(io::IO, e::SealMismatchError) =
    print(io, "SotLog seal verification failed: ", e.reason)

# ---------------------------------------------------------------------------
# Record wrapper constants (ADR 0039 "Record wrapper").
# ---------------------------------------------------------------------------

const MAGIC = (0xA9, 0x5F)
const WRAPPER_VERSION = 0x01
const CODEC_JSON = 0x01
const PRELUDE_LEN = 18
const RECORD_MAX_BODY = UInt32(16 * 1024 * 1024)

@enum RecordKind::UInt8 begin
    HEADER_KIND = 1
    FRAME_KIND = 2
    SEAL_KIND = 3
end

function record_kind_from_byte(b::UInt8)
    b == 0x01 && return HEADER_KIND
    b == 0x02 && return FRAME_KIND
    b == 0x03 && return SEAL_KIND
    return nothing
end

"One decoded record: its kind, body bytes, and where it sat in the file."
struct Record
    kind::RecordKind
    body::Vector{UInt8}
    offset::Int      # 1-based offset of the record's first (magic) byte
    wire_len::Int     # prelude (18) + body length
end

le_u32(b::AbstractVector{UInt8}) =
    UInt32(b[1]) | (UInt32(b[2]) << 8) | (UInt32(b[3]) << 16) | (UInt32(b[4]) << 24)

"""
    decode_at(buf, offset) -> Record

Decode one record starting at 1-based `offset` in `buf`. Caller guarantees
`offset <= length(buf)` (i.e. there is at least one byte to look at) — the
"clean end of file" case is the loop's responsibility, not this function's.

Field checks run in the same order as the reference `record.rs`: magic,
version, kind, codec, reserved, *then* the prelude CRC — so a single
corrupted field is reported as itself even when its own CRC has been
patched to match (the fail-closed tests below rely on this).
"""
function decode_at(buf::Vector{UInt8}, offset::Int)::Record
    n = length(buf)
    remaining = n - offset + 1
    if remaining < PRELUDE_LEN
        throw(TornTailError(offset, "truncated prelude"))
    end
    prelude = @view buf[offset:offset+PRELUDE_LEN-1]

    if (prelude[1], prelude[2]) != MAGIC
        throw(CorruptRecordError(offset, "bad magic"))
    end
    version = prelude[3]
    if version != WRAPPER_VERSION
        throw(CorruptRecordError(offset, "unknown wrapper version $(version)"))
    end
    kind = record_kind_from_byte(prelude[4])
    if kind === nothing
        throw(CorruptRecordError(offset, "unknown record kind $(prelude[4])"))
    end
    codec = prelude[5]
    if codec != CODEC_JSON
        throw(CorruptRecordError(offset, "unknown codec id $(codec)"))
    end
    reserved = prelude[6]
    if reserved != 0
        throw(CorruptRecordError(offset, "nonzero reserved byte $(reserved)"))
    end

    stored_prelude_crc = le_u32(@view prelude[11:14])
    computed_prelude_crc = crc32c(collect(@view prelude[3:10]))
    if stored_prelude_crc != computed_prelude_crc
        throw(CorruptRecordError(offset, "prelude crc mismatch"))
    end

    len = le_u32(@view prelude[7:10])
    if len > RECORD_MAX_BODY
        throw(CorruptRecordError(offset, "len $(len) exceeds cap"))
    end
    body_start = offset + PRELUDE_LEN
    body_end = body_start + Int(len) - 1
    if body_end > n
        throw(TornTailError(offset, "torn body"))
    end
    body = buf[body_start:body_end]
    stored_body_crc = le_u32(@view prelude[15:18])
    if crc32c(body) != stored_body_crc
        throw(CorruptRecordError(offset, "body crc mismatch"))
    end

    return Record(kind, body, offset, PRELUDE_LEN + Int(len))
end

# ---------------------------------------------------------------------------
# Segment-level record stream parse (ADR 0039 "Segment lifecycle" — record
# order `header frame* seal?`, and "the writer never appends after sealing —
# a post-seal suffix is loud in every state").
# ---------------------------------------------------------------------------

"""
    parse_records(bytes) -> (header_rec, frame_recs, seal_rec)

Walk the record stream once, enforcing: header first and only once, no
frame/seal before the header, and — since this reader only ever consumes
already-sealed `.sotseg` files — nothing at all after the seal record. Any
byte found once `seal_rec` is set throws [`CorruptRecordError`](@ref)
`"record after seal"` immediately, without attempting to decode it (a
strict superset of the ADR's tear-vs-loud distinction, which only matters
for the writer-side `.open`/`.recovering` states this reader doesn't
implement).
"""
function parse_records(bytes::Vector{UInt8})
    n = length(bytes)
    offset = 1
    header_rec = nothing
    frame_recs = Record[]
    seal_rec = nothing
    while offset <= n
        if seal_rec !== nothing
            throw(CorruptRecordError(offset, "record after seal"))
        end
        rec = decode_at(bytes, offset)
        if rec.kind == HEADER_KIND
            if header_rec !== nothing
                throw(CorruptRecordError(offset, "second header record"))
            end
            header_rec = rec
        elseif rec.kind == FRAME_KIND
            if header_rec === nothing
                throw(CorruptRecordError(offset, "frame before header"))
            end
            push!(frame_recs, rec)
        else # SEAL_KIND
            if header_rec === nothing
                throw(CorruptRecordError(offset, "seal before header"))
            end
            seal_rec = rec
        end
        offset += rec.wire_len
    end
    if header_rec === nothing
        throw(CorruptRecordError(1, "missing header record"))
    end
    if seal_rec === nothing
        throw(CorruptRecordError(n + 1, "sealed segment without a seal"))
    end
    return header_rec, frame_recs, seal_rec
end

record_wire_bytes(bytes::Vector{UInt8}, rec::Record) =
    bytes[rec.offset:rec.offset+rec.wire_len-1]

# ---------------------------------------------------------------------------
# Typed bodies (ADR 0039 "Segment lifecycle": header body / seal body).
# ---------------------------------------------------------------------------

"""
`{algo, value}` — ADR 0039 `Digest`. Only `algo = "sha256"` exists in v1.
"""
struct Digest
    algo::String
    value::String
end
Base.:(==)(a::Digest, b::Digest) = a.algo == b.algo && a.value == b.value

"`{epoch, n}` — ADR 0039 `Ref` / a frame's `seq`."
struct Seq
    epoch::Int
    n::Int
end
Base.:(==)(a::Seq, b::Seq) = a.epoch == b.epoch && a.n == b.n

const RETENTION_CLASSES = ("archive", "discard", "distill")

"ADR 0039 feature registry (amended 2026-08-24) — unknown features FAIL CLOSED."
const REGISTERED_FEATURES = ("sot.producer.json-f64-v1", "sot.capsule.cgroup-fence-v1")

"""
    HeaderBody

Parsed segment header record body (ADR 0039 "Header body"). `retention_class`
is genesis-only (`segment_index == 0`); `nothing` otherwise.
"""
struct HeaderBody
    version::Int
    required_features::Vector{String}
    voyage_id::String
    segment_index::Int
    epoch::Int
    prev_seal_digest::Union{Digest,Nothing}
    created_wall_ms::Int64
    retention_class::Union{String,Nothing}
end

"""
    SealBody

Parsed seal record body (ADR 0039 "Seal body"). `nothing` fields are exactly
the ADR's optional/genesis-only fields; `first_seq`/`last_seq` are `nothing`
only for an empty segment.
"""
struct SealBody
    frame_count::Int
    first_seq::Union{Seq,Nothing}
    last_seq::Union{Seq,Nothing}
    recovered::Union{Bool,Nothing}
    truncated_bytes::Union{Int,Nothing}
    truncation_reason::Union{String,Nothing}
    recovered_by_epoch::Union{Int,Nothing}
    digest::Digest
end

# Small helper: JSON3.Object behaves like an AbstractDict but plain `get`
# with a Symbol key needs a `haskey` guard to stay ignorable-unknown-safe in
# both directions (present-with-null vs. genuinely absent).
jget(obj, key::Symbol, default) = haskey(obj, key) ? obj[key] : default

function parse_seq(obj)::Seq
    haskey(obj, :epoch) || throw(SchemaError("ref/seq missing epoch"))
    haskey(obj, :n) || throw(SchemaError("ref/seq missing n"))
    Seq(Int(obj.epoch), Int(obj.n))
end
parse_seq_opt(x) = x === nothing ? nothing : parse_seq(x)

function parse_digest(obj)::Digest
    haskey(obj, :algo) || throw(SchemaError("digest missing algo"))
    haskey(obj, :value) || throw(SchemaError("digest missing value"))
    Digest(String(obj.algo), String(obj.value))
end

"""
    parse_header_body(body::Vector{UInt8}) -> HeaderBody

Parse a header record's JSON body per ADR 0039. Unknown JSON object members
are ignored (parsed field-by-field rather than via strict struct binding).
"""
function parse_header_body(body::Vector{UInt8})::HeaderBody
    obj = JSON3.read(body)
    haskey(obj, :version) || throw(SchemaError("header missing version"))
    haskey(obj, :voyage_id) || throw(SchemaError("header missing voyage_id"))
    haskey(obj, :segment_index) || throw(SchemaError("header missing segment_index"))
    haskey(obj, :epoch) || throw(SchemaError("header missing epoch"))
    haskey(obj, :created_wall_ms) || throw(SchemaError("header missing created_wall_ms"))

    required_features = String[String(s) for s in jget(obj, :required_features, [])]
    for f in required_features
        f in REGISTERED_FEATURES ||
            throw(SchemaError("segment requires unknown feature $(repr(f))"))
    end
    prev = jget(obj, :prev_seal_digest, nothing)
    prev_digest = prev === nothing ? nothing : parse_digest(prev)
    retention_class = jget(obj, :retention_class, nothing)
    retention_class = retention_class === nothing ? nothing : String(retention_class)
    if retention_class !== nothing && !(retention_class in RETENTION_CLASSES)
        throw(SchemaError("unknown retention_class $(repr(retention_class))"))
    end

    HeaderBody(
        Int(obj.version),
        required_features,
        String(obj.voyage_id),
        Int(obj.segment_index),
        Int(obj.epoch),
        prev_digest,
        Int64(obj.created_wall_ms),
        retention_class,
    )
end

"""
    parse_seal_body(body::Vector{UInt8}) -> SealBody

Parse a seal record's JSON body per ADR 0039. Unknown JSON object members
are ignored.
"""
function parse_seal_body(body::Vector{UInt8})::SealBody
    obj = JSON3.read(body)
    haskey(obj, :frame_count) || throw(SchemaError("seal missing frame_count"))
    haskey(obj, :digest) || throw(SchemaError("seal missing digest"))

    recovered = jget(obj, :recovered, nothing)
    recovered = recovered === nothing ? nothing : Bool(recovered)
    truncated_bytes = jget(obj, :truncated_bytes, nothing)
    truncated_bytes = truncated_bytes === nothing ? nothing : Int(truncated_bytes)
    truncation_reason = jget(obj, :truncation_reason, nothing)
    truncation_reason = truncation_reason === nothing ? nothing : String(truncation_reason)
    recovered_by_epoch = jget(obj, :recovered_by_epoch, nothing)
    recovered_by_epoch = recovered_by_epoch === nothing ? nothing : Int(recovered_by_epoch)

    SealBody(
        Int(obj.frame_count),
        parse_seq_opt(jget(obj, :first_seq, nothing)),
        parse_seq_opt(jget(obj, :last_seq, nothing)),
        recovered,
        truncated_bytes,
        truncation_reason,
        recovered_by_epoch,
        parse_digest(obj.digest),
    )
end

# ---------------------------------------------------------------------------
# Frame validation (ADR 0039 "Frame envelope and classes"). Frames stay as
# JSON3.Object — this reader validates identity/class only, not the full
# cross-field matrix (verifier-decidable rules out of scope here).
# ---------------------------------------------------------------------------

"The eight closed frame classes (ADR 0039 `Envelope.class`)."
const FRAME_CLASSES = (
    "input", "turn_open", "turn_close", "control_exchange",
    "artifact_ref", "lifecycle", "producer_attached", "producer",
)

"""
    validate_frame(obj::JSON3.Object)

Check the identity fields a frame must carry: `seq.epoch`/`seq.n` present,
`seq.n >= 1`, and `class` one of the eight ADR 0039 values. Throws
[`SchemaError`](@ref) on any violation — the class check is a closed enum,
so an unrecognized class is a fail-closed error, not a silently-ignored
unknown field.
"""
function validate_frame(obj)
    haskey(obj, :seq) || throw(SchemaError("frame missing seq"))
    seq = parse_seq(obj.seq)
    seq.n >= 1 || throw(SchemaError("seq.n must be >= 1, got $(seq.n)"))
    haskey(obj, :class) || throw(SchemaError("frame missing class"))
    class = String(obj.class)
    class in FRAME_CLASSES || throw(SchemaError("unknown frame class $(repr(class))"))
    return nothing
end

frame_seq(obj) = parse_seq(obj.seq)

# ---------------------------------------------------------------------------
# Segment
# ---------------------------------------------------------------------------

"""
    Segment

A fully parsed, structurally validated `.sotseg` segment file.

- `header::HeaderBody`
- `frames::Vector{JSON3.Object}` — each has passed [`validate_frame`](@ref)
- `seal::SealBody` — always present; this reader only reads sealed segments
"""
struct Segment
    header::HeaderBody
    frames::Vector{JSON3.Object}
    seal::SealBody
end

"""
    read_segment(path) -> Segment

Parse a `.sotseg` file at `path` per ADR 0039's record wrapper and segment
lifecycle. Fails closed (throws a [`SotLogError`](@ref) subtype) on any
wire-format or structural violation — see [`CorruptRecordError`](@ref),
[`TornTailError`](@ref), and [`SchemaError`](@ref).
"""
read_segment(path::AbstractString)::Segment = read_segment_bytes(read(path))

"""
    read_segment(bytes::Vector{UInt8}) -> Segment

Dispatch twin of [`read_segment`](@ref) for an in-memory byte buffer — the
entry point the corruption tests use to mutate a copy of the fixture without
touching disk. Identical to `read_segment_bytes`, kept as a separate name
for callers that already hold a `Vector{UInt8}` and want the file-agnostic
verb.
"""
read_segment(bytes::Vector{UInt8})::Segment = read_segment_bytes(bytes)

"""
    read_segment_bytes(bytes::Vector{UInt8}) -> Segment

As [`read_segment`](@ref), but operating on an in-memory byte buffer — the
entry point the corruption tests use to mutate a copy of the fixture.
"""
function read_segment_bytes(bytes::Vector{UInt8})::Segment
    header_rec, frame_recs, seal_rec = parse_records(bytes)
    header = parse_header_body(header_rec.body)
    frames = JSON3.Object[]
    for rec in frame_recs
        obj = JSON3.read(rec.body)
        validate_frame(obj)
        push!(frames, obj)
    end
    seal = parse_seal_body(seal_rec.body)
    return Segment(header, frames, seal)
end

# ---------------------------------------------------------------------------
# Seal digest verification (ADR 0039 "Encoding atoms": seal-digest preimage).
# ---------------------------------------------------------------------------

const SEAL_DOMAIN = Vector{UInt8}("sotseg1.seal\0")
const BODY_CRC_OFFSET = 15  # 1-based start of the body_crc32c prelude field
const DIGEST_VALUE_NEEDLE = Vector{UInt8}("\"value\":\"")
const DIGEST_VALUE_LEN = 64

hex_lower(bytes::AbstractVector{UInt8}) =
    join(lpad(string(b; base=16), 2, '0') for b in bytes)

"""
    digest_value_range(body) -> UnitRange

1-based byte range of the seal digest's 64 hex characters within a seal
record's body — the *last* `"value":"` occurrence (the digest field is last
by construction; ADR 0039 encoding atoms).
"""
function digest_value_range(body::AbstractVector{UInt8})
    nl = length(DIGEST_VALUE_NEEDLE)
    pos = nothing
    i = 1
    while i + nl - 1 <= length(body)
        if @view(body[i:i+nl-1]) == DIGEST_VALUE_NEEDLE
            pos = i + nl
        end
        i += 1
    end
    pos === nothing && throw(SchemaError("seal body has no digest value"))
    if pos + DIGEST_VALUE_LEN - 1 > length(body)
        throw(SchemaError("seal digest value truncated"))
    end
    return pos:(pos+DIGEST_VALUE_LEN-1)
end

"""
    seal_record_preimage(seal_wire) -> Vector{UInt8}

The seal RECORD's wire bytes with the two length-preserving, in-place
substitutions the ADR's seal-digest preimage requires: the digest value's
64 hex characters → 64 ASCII `'0'`, and the record's `body_crc32c` prelude
field → zero.
"""
function seal_record_preimage(seal_wire::AbstractVector{UInt8})
    w = collect(seal_wire)
    w[BODY_CRC_OFFSET:BODY_CRC_OFFSET+3] .= 0x00
    body = @view w[PRELUDE_LEN+1:end]
    rng = digest_value_range(body)
    w[PRELUDE_LEN .+ rng] .= UInt8('0')
    return w
end

"""
    verify_seal(path_or_bytes) -> true

Recompute the segment's seal digest from its raw wire bytes and compare it
against the stored digest (ADR 0039 seal-digest preimage: sha256 over the
domain string, the header record's wire bytes, every frame record's wire
bytes in order, then the seal record's wire bytes with the two in-place
substitutions above). Also checks that the seal's `frame_count`/`first_seq`/
`last_seq` agree with the parsed frames. Returns `true` on success; throws
[`SealMismatchError`](@ref) (or a parse error from [`read_segment_bytes`](@ref)'s
underlying record walk) on any disagreement.

Accepts either a path (`AbstractString`) or an in-memory `Vector{UInt8}`.
"""
function verify_seal(path_or_bytes)::Bool
    bytes = path_or_bytes isa AbstractVector{UInt8} ? path_or_bytes : read(path_or_bytes)
    header_rec, frame_recs, seal_rec = parse_records(bytes)
    seal = parse_seal_body(seal_rec.body)

    ctx = SHA256_CTX()
    update!(ctx, SEAL_DOMAIN)
    update!(ctx, record_wire_bytes(bytes, header_rec))
    for rec in frame_recs
        update!(ctx, record_wire_bytes(bytes, rec))
    end
    update!(ctx, seal_record_preimage(record_wire_bytes(bytes, seal_rec)))
    computed_hex = hex_lower(digest!(ctx))

    if seal.digest.algo != "sha256" || seal.digest.value != computed_hex
        throw(SealMismatchError("seal digest mismatch"))
    end

    frame_seqs = map(rec -> frame_seq(JSON3.read(rec.body)), frame_recs)
    actual_first = isempty(frame_seqs) ? nothing : first(frame_seqs)
    actual_last = isempty(frame_seqs) ? nothing : last(frame_seqs)
    if seal.frame_count != length(frame_recs) ||
       seal.first_seq != actual_first ||
       seal.last_seq != actual_last
        throw(SealMismatchError("seal metadata disagrees with frames"))
    end

    return true
end

end # module SotLog
