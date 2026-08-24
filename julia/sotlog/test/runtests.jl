using Test
using CRC32c: crc32c
using JSON3
using SotLog

# Reach the non-exported record-walk internals for whitebox corruption tests
# (locating record offsets robustly instead of hardcoding byte positions).
const SL = SotLog

# ---------------------------------------------------------------------------
# Locate the golden fixture: rust/log/tests/fixtures/golden-v1.sotseg,
# walking up from this file to the repo root (robust to worktree layout).
# ---------------------------------------------------------------------------

function find_repo_root(start::AbstractString)
    dir = abspath(start)
    while true
        candidate = joinpath(dir, "rust", "log", "tests", "fixtures")
        if isdir(candidate)
            return dir
        end
        parent = dirname(dir)
        if parent == dir
            error("could not locate a directory containing rust/log/tests/fixtures walking up from $(start)")
        end
        dir = parent
    end
end

const REPO_ROOT = find_repo_root(@__DIR__)
const FIXTURE_PATH = joinpath(REPO_ROOT, "rust", "log", "tests", "fixtures", "golden-v1.sotseg")

@assert isfile(FIXTURE_PATH) "golden fixture not found at $(FIXTURE_PATH)"

# ---------------------------------------------------------------------------
# Small byte-poking helpers for the corruption tests below.
# ---------------------------------------------------------------------------

"Write `v` little-endian into `bytes[at:at+3]` (1-based)."
function poke_le_u32!(bytes::Vector{UInt8}, at::Int, v::UInt32)
    bytes[at]   = UInt8(v & 0xff)
    bytes[at+1] = UInt8((v >> 8) & 0xff)
    bytes[at+2] = UInt8((v >> 16) & 0xff)
    bytes[at+3] = UInt8((v >> 24) & 0xff)
end

"Recompute and rewrite record CRC-prelude field over bytes[offset+2:offset+9] (version..len)."
function refix_prelude_crc!(bytes::Vector{UInt8}, record_offset::Int)
    crc = crc32c(bytes[record_offset+2:record_offset+9])
    poke_le_u32!(bytes, record_offset + 10, crc)
end

@testset "SotLog" begin

    @testset "1. fixture parses" begin
        seg = read_segment(FIXTURE_PATH)

        @test seg.header.voyage_id == "01900000-0000-7000-8000-000000000001"
        @test seg.header.segment_index == 0
        @test seg.header.epoch == 1
        @test seg.header.retention_class == "archive"
        @test seg.header.version == 1
        @test seg.header.required_features == String[]
        @test seg.header.prev_seal_digest === nothing

        @test length(seg.frames) == 3
        @test [String(f.class) for f in seg.frames] ==
              ["producer_attached", "lifecycle", "producer"]

        f3 = seg.frames[3]
        @test f3.payload.native.text == "hello, log"

        attached = [r for r in f3.refs if String(r.kind) == "attached_to"]
        @test length(attached) == 1
        @test Int(attached[1].frame.epoch) == 1
        @test Int(attached[1].frame.n) == 1

        @test seg.seal.frame_count == 3
        @test seg.seal.first_seq == SotLog.Seq(1, 1)
        @test seg.seal.last_seq == SotLog.Seq(1, 3)
    end

    @testset "2. verify_seal passes on the pristine fixture" begin
        @test verify_seal(FIXTURE_PATH) === true
        # Also works directly on bytes (segment_bytes_or_path).
        @test verify_seal(read(FIXTURE_PATH)) === true
    end

    @testset "3. single-byte corruption of an in-memory copy always throws" begin
        bytes0 = read(FIXTURE_PATH)
        header_rec, frame_recs, seal_rec = SL.parse_records(bytes0)
        frame3 = frame_recs[3]

        @testset "3a. frame body byte, CRC uncorrected -> CRC error" begin
            bad = copy(bytes0)
            body_start = frame3.offset + SL.PRELUDE_LEN
            bad[body_start] ⊻= 0x01
            @test_throws SL.CorruptRecordError read_segment(bad)
        end

        @testset "3b. frame body byte, record CRC fixed -> verify_seal digest mismatch" begin
            bad = copy(bytes0)
            body_start = frame3.offset + SL.PRELUDE_LEN
            body_str = String(copy(frame3.body))
            rng = findfirst("hello, log", body_str)
            rng === nothing && error("fixture no longer contains the expected payload text")
            rel = first(rng)
            target = body_start + rel - 1
            bad[target] ⊻= 0x01   # 'h' (0x68) -> 'i' (0x69): stays ASCII, JSON stays valid.

            body_end = body_start + length(frame3.body) - 1
            new_body_crc = crc32c(bad[body_start:body_end])
            poke_le_u32!(bad, frame3.offset + 14, new_body_crc)

            # The record itself now parses cleanly (CRCs are internally
            # consistent) — only the seal's hash chain notices the change.
            seg = read_segment(bad)
            @test seg.frames[3].payload.native.text != "hello, log"
            @test_throws SL.SealMismatchError verify_seal(bad)
        end

        @testset "3c. truncating the final record -> a distinct torn-tail error" begin
            # (i) cut into the seal record's BODY (valid 18-byte prelude,
            # insufficient body bytes remaining).
            torn_body = bytes0[1:end-5]
            @test_throws SL.TornTailError read_segment(torn_body)

            # (ii) cut into the seal record's PRELUDE itself (fewer than 18
            # bytes remain for the final record at all).
            torn_prelude = bytes0[1:seal_rec.offset+4]
            @test_throws SL.TornTailError read_segment(torn_prelude)

            # Torn-tail is a genuinely distinct exception type, not a
            # generic corruption error.
            local threw_type = nothing
            try
                read_segment(torn_body)
            catch e
                threw_type = typeof(e)
            end
            @test threw_type == SL.TornTailError
            @test threw_type != SL.CorruptRecordError
        end

        @testset "3d. garbage appended after the seal -> record-after-seal error" begin
            garbage = vcat(copy(bytes0), Vector{UInt8}("garbage"))
            err = nothing
            try
                read_segment(garbage)
            catch e
                err = e
            end
            @test err isa SL.CorruptRecordError
            @test occursin("record after seal", err.reason)
        end
    end

    @testset "4. fail-closed: single wrapper field corrupted, prelude CRC fixed" begin
        bytes0 = read(FIXTURE_PATH)
        header_rec, _, _ = SL.parse_records(bytes0)

        # 1-based offsets of version/kind/codec/reserved within the prelude
        # (relative 3,4,5,6 -> absolute offset+2,+3,+4,+5).
        field_rel_positions = Dict(
            :version  => 3,
            :kind     => 4,
            :codec    => 5,
            :reserved => 6,
        )

        for (name, relpos) in field_rel_positions
            bad = copy(bytes0)
            abspos = header_rec.offset + relpos - 1
            bad[abspos] = 0x09   # an invalid value for every one of these fields
            refix_prelude_crc!(bad, header_rec.offset)
            @test_throws SL.CorruptRecordError read_segment(bad)
        end
    end

    @testset "5. frame schema: seq/class closed-enum checks (not exercised above)" begin
        good = JSON3.read("""{"seq":{"epoch":1,"n":1},"class":"producer","refs":[]}""")
        @test SL.validate_frame(good) === nothing

        no_seq = JSON3.read("""{"class":"producer","refs":[]}""")
        @test_throws SL.SchemaError SL.validate_frame(no_seq)

        n_zero = JSON3.read("""{"seq":{"epoch":1,"n":0},"class":"producer","refs":[]}""")
        @test_throws SL.SchemaError SL.validate_frame(n_zero)

        unknown_class = JSON3.read("""{"seq":{"epoch":1,"n":1},"class":"brand_new_class","refs":[]}""")
        @test_throws SL.SchemaError SL.validate_frame(unknown_class)
    end


    @testset "6. f64 golden: registered feature accepted, fractional numbers round-trip" begin
        f64_path = joinpath(REPO_ROOT, "rust", "log", "tests", "fixtures", "golden-f64-v1.sotseg")
        @assert isfile(f64_path)
        seg = read_segment(f64_path)
        @test seg.header.required_features == ["sot.producer.json-f64-v1"]
        @test verify_seal(f64_path) === true
        f3 = seg.frames[3]
        @test f3.payload.total_cost_usd ≈ 0.048731
        @test f3.payload.tiny ≈ 1.5e-8
        @test Int64(f3.payload.edge) == 9007199254740991
    end

    @testset "7. unknown required feature fails closed" begin
        bytes0 = read(FIXTURE_PATH)
        hdr, _, _ = SL.parse_records(bytes0)
        body_str = String(copy(hdr.body))
        new_body = replace(body_str, "\"required_features\":[]" =>
            "\"required_features\":[\"sot.future.x-v9\"]")
        @test new_body != body_str
        nb = Vector{UInt8}(new_body)
        prelude = zeros(UInt8, 18)
        prelude[1] = 0xA9; prelude[2] = 0x5F
        prelude[3] = 0x01; prelude[4] = 0x01; prelude[5] = 0x01; prelude[6] = 0x00
        len = UInt32(length(nb))
        prelude[7] = UInt8(len & 0xff); prelude[8] = UInt8((len >> 8) & 0xff)
        prelude[9] = UInt8((len >> 16) & 0xff); prelude[10] = UInt8((len >> 24) & 0xff)
        pcrc = crc32c(prelude[3:10])
        prelude[11] = UInt8(pcrc & 0xff); prelude[12] = UInt8((pcrc >> 8) & 0xff)
        prelude[13] = UInt8((pcrc >> 16) & 0xff); prelude[14] = UInt8((pcrc >> 24) & 0xff)
        bcrc = crc32c(nb)
        prelude[15] = UInt8(bcrc & 0xff); prelude[16] = UInt8((bcrc >> 8) & 0xff)
        prelude[17] = UInt8((bcrc >> 16) & 0xff); prelude[18] = UInt8((bcrc >> 24) & 0xff)
        rec = SL.decode_at(vcat(prelude, nb), 1)
        @test rec.kind == SL.HEADER_KIND
        @test_throws SL.SchemaError SL.parse_header_body(rec.body)
    end
end
