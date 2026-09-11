//! Black-box tests for `sot_log::wire` (ADR 0041 step 5, unit U1) that
//! exercise it purely through its public API, as a real caller (the
//! capsule, a test client, step 6's client) would: build frames, encode
//! them, and feed the resulting bytes through a [`FrameSplitter`] however
//! they happen to arrive.
//!
//! Byte-level goldens, bounds, lane binding, and the chunk-arithmetic
//! proof live as unit tests inside `src/wire.rs` itself (matching
//! `host_handshake.rs`'s discipline of colocating tests with the module
//! they exercise). What belongs here instead is scenario coverage that
//! is inherently about the PUBLIC surface: arbitrary chunking of a
//! multi-frame stream, and fuzzing -- plus an exception mirroring
//! `tests/golden.rs`'s own pattern: `supervisor_lane_v1_bytes_are_pinned`
//! and `supervisor_lane_v1_coverage_bytes_are_pinned_and_decode_back_identically`
//! together pin the WHOLE supervisor lane's v1 wire bytes -- every
//! top-level and nested variant, every optional field both absent and
//! present -- against two immutable committed fixture files (ADR 0045
//! decision 7), the file-based conformance proof `src/wire.rs`'s own
//! per-frame inline-byte goldens don't provide.

use sot_log::wire::{
    encode_attach_client, encode_attach_server, encode_keepalive, encode_mgmt_reply,
    encode_mgmt_request, encode_supervisor_reply, encode_supervisor_request, AttachClient,
    AttachRefusedReason, AttachServer, DecodedFrame, FrameSplitter, MgmtReply, MgmtRequest,
    ResizeRefusedReason, SupervisorOp, SupervisorOperationState, SupervisorPhase,
    SupervisorRefusedReason, SupervisorReply, SupervisorRequest, Survival, TakeRefusedReason,
    WireError, MGMT_MAGIC, SUPERVISOR_PROTO_V1,
};

/// xorshift64* -- a fixed seed reproduces a failure exactly, with the
/// seed printed alongside it. Same generator shape as
/// `rust/vt100/tests/roundtrip_fuzz.rs`.
struct Rng(u64);

impl Rng {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }

    fn bool(&mut self) -> bool {
        self.below(2) == 0
    }
}

fn random_ascii_string(rng: &mut Rng, max_len: usize) -> String {
    let len = rng.below((max_len + 1) as u32) as usize;
    (0..len)
        .map(|_| (b'a' + (rng.below(26) as u8)) as char)
        .collect()
}

/// Like `random_ascii_string`, but never empty -- for `controller_id`,
/// which the wire refuses at length 0 (see the module doc's "Field
/// minimums" section).
fn random_nonempty_ascii_string(rng: &mut Rng, max_len: usize) -> String {
    let len = 1 + rng.below(max_len as u32) as usize;
    (0..len)
        .map(|_| (b'a' + (rng.below(26) as u8)) as char)
        .collect()
}

fn random_bytes(rng: &mut Rng, max_len: usize) -> Vec<u8> {
    let len = rng.below((max_len + 1) as u32) as usize;
    (0..len).map(|_| rng.below(256) as u8).collect()
}

fn random_idem_key(rng: &mut Rng) -> [u8; 16] {
    let mut key = [0u8; 16];
    for b in &mut key {
        *b = rng.below(256) as u8;
    }
    key
}

/// One frame, tagged by which encoder it rides. `Keepalive` is its own
/// variant, not nested under `AttachClient`/`AttachServer`: the wire has
/// exactly one direction-neutral `keepalive` shape (see `wire`'s module
/// doc), encoded by the single `encode_keepalive` function.
#[derive(Debug, Clone)]
enum GeneratedFrame {
    MgmtRequest(MgmtRequest),
    MgmtReply(MgmtReply),
    AttachClient(AttachClient),
    AttachServer(AttachServer),
    Keepalive(u64),
    SupervisorRequest(SupervisorRequest),
    SupervisorReply(SupervisorReply),
}

impl GeneratedFrame {
    fn encode(&self) -> Vec<u8> {
        match self {
            Self::MgmtRequest(f) => encode_mgmt_request(f).expect("generator stays in-bounds"),
            Self::MgmtReply(f) => encode_mgmt_reply(f).expect("generator stays in-bounds"),
            Self::AttachClient(f) => encode_attach_client(f).expect("generator stays in-bounds"),
            Self::AttachServer(f) => encode_attach_server(f).expect("generator stays in-bounds"),
            Self::Keepalive(nonce) => encode_keepalive(*nonce),
            Self::SupervisorRequest(f) => encode_supervisor_request(f).expect("generator stays in-bounds"),
            Self::SupervisorReply(f) => encode_supervisor_reply(f).expect("generator stays in-bounds"),
        }
    }

    fn as_decoded(&self) -> DecodedFrame {
        match self {
            Self::MgmtRequest(f) => DecodedFrame::MgmtRequest(f.clone()),
            Self::MgmtReply(f) => DecodedFrame::MgmtReply(f.clone()),
            Self::AttachClient(f) => DecodedFrame::AttachClient(f.clone()),
            Self::AttachServer(f) => DecodedFrame::AttachServer(f.clone()),
            Self::Keepalive(nonce) => DecodedFrame::Keepalive { nonce: *nonce },
            Self::SupervisorRequest(f) => DecodedFrame::SupervisorRequest(f.clone()),
            Self::SupervisorReply(f) => DecodedFrame::SupervisorReply(f.clone()),
        }
    }
}

fn random_mgmt_frame(rng: &mut Rng) -> GeneratedFrame {
    if rng.bool() {
        let f = match rng.below(3) {
            0 => MgmtRequest::Probe,
            1 => MgmtRequest::Status,
            _ => MgmtRequest::Shutdown {
                reason: random_ascii_string(rng, 128),
            },
        };
        GeneratedFrame::MgmtRequest(f)
    } else {
        let f = match rng.below(3) {
            0 => MgmtReply::ProbeOk,
            1 => MgmtReply::StatusOk {
                pid: rng.next_u32(),
                created: (u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32()),
                survival: if rng.bool() {
                    Survival::Normal
                } else {
                    Survival::Degraded
                },
            },
            _ => MgmtReply::ShutdownOk,
        };
        GeneratedFrame::MgmtReply(f)
    }
}

fn random_attach_frame(rng: &mut Rng) -> GeneratedFrame {
    match rng.below(3) {
        0 => {
            let f = match rng.below(5) {
                0 => AttachClient::Hello {
                    proto: rng.next_u32(),
                },
                1 => AttachClient::Attach {
                    controller_id: random_nonempty_ascii_string(rng, 128),
                },
                2 => AttachClient::Take {
                    controller_id: random_nonempty_ascii_string(rng, 128),
                },
                3 => AttachClient::Input {
                    controller_id: random_nonempty_ascii_string(rng, 128),
                    take_epoch: (u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32()),
                    idem_key: random_idem_key(rng),
                    payload: random_bytes(rng, 200), // small -- bounds are tested separately
                },
                _ => AttachClient::Resize {
                    cols: rng.below(513) as u16,
                    rows: rng.below(257) as u16,
                },
            };
            GeneratedFrame::AttachClient(f)
        }
        1 => {
            let f = match rng.below(12) {
                0 => AttachServer::HelloOk {
                    proto: rng.next_u32(),
                },
                1 => AttachServer::HelloRefused {
                    supported: rng.next_u32(),
                },
                2 => AttachServer::CheckpointChunk {
                    last: rng.bool(),
                    bytes: random_bytes(rng, 300),
                },
                3 => AttachServer::AttachRefused {
                    reason: if rng.bool() {
                        AttachRefusedReason::GroundTimeout
                    } else {
                        AttachRefusedReason::SubscriberCap
                    },
                },
                4 => AttachServer::Output {
                    bytes: random_bytes(rng, 300),
                },
                5 => AttachServer::TakeOk {
                    take_epoch: (u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32()),
                },
                6 => AttachServer::TakeRefused {
                    reason: if rng.bool() {
                        TakeRefusedReason::NotAttached
                    } else {
                        TakeRefusedReason::CheckpointInFlight
                    },
                },
                7 => AttachServer::InputRecorded,
                8 => AttachServer::InputRefusedStale,
                9 => AttachServer::InputDeliveryUnknown,
                10 => AttachServer::ResizeOk,
                _ => AttachServer::ResizeRefused {
                    reason: if rng.bool() {
                        ResizeRefusedReason::OutOfBudget
                    } else {
                        ResizeRefusedReason::NotDriver
                    },
                },
            };
            GeneratedFrame::AttachServer(f)
        }
        _ => GeneratedFrame::Keepalive(
            (u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32()),
        ),
    }
}

/// A random `end_run | reset | stop`. `voyage`/`operation_id` fields
/// elsewhere are generated via `random_nonempty_ascii_string`, which only
/// ever emits lowercase `a`-`z` — already a subset of `operation_id`'s
/// own `[A-Za-z0-9._-]` charset, so no separate generator is needed for
/// THAT bound; `operation_id`'s LENGTH bound (64, not the general 128) is
/// what callers must pass explicitly.
fn random_supervisor_op(rng: &mut Rng) -> SupervisorOp {
    match rng.below(3) {
        0 => SupervisorOp::EndRun {
            reason: random_ascii_string(rng, 128),
            voyage: random_nonempty_ascii_string(rng, 128),
        },
        1 => SupervisorOp::Reset {
            voyage: if rng.bool() { Some(random_nonempty_ascii_string(rng, 128)) } else { None },
        },
        _ => SupervisorOp::Stop,
    }
}

fn random_supervisor_operation_state(rng: &mut Rng) -> SupervisorOperationState {
    match rng.below(8) {
        0 => SupervisorOperationState::Accepted,
        1 => SupervisorOperationState::RecordClosed,
        2 => SupervisorOperationState::RecordVerified,
        3 => SupervisorOperationState::ResetDone { new_voyage: random_nonempty_ascii_string(rng, 128) },
        4 => SupervisorOperationState::Stopping,
        5 => SupervisorOperationState::Failed { detail: random_ascii_string(rng, 128) },
        6 => SupervisorOperationState::Refused {
            reason: if rng.bool() { SupervisorRefusedReason::StaleVoyage } else { SupervisorRefusedReason::IdConflict },
        },
        _ => SupervisorOperationState::UnknownOperation,
    }
}

fn random_supervisor_frame(rng: &mut Rng) -> GeneratedFrame {
    if rng.bool() {
        let f = match rng.below(4) {
            0 => SupervisorRequest::Hello { proto: rng.next_u32(), build: random_ascii_string(rng, 128) },
            1 => SupervisorRequest::Command {
                operation_id: random_nonempty_ascii_string(rng, 64),
                op: random_supervisor_op(rng),
            },
            2 => SupervisorRequest::Status,
            _ => SupervisorRequest::Query { operation_id: random_nonempty_ascii_string(rng, 64) },
        };
        GeneratedFrame::SupervisorRequest(f)
    } else {
        let f = match rng.below(4) {
            0 => SupervisorReply::HelloOk {
                proto: rng.next_u32(),
                build: random_ascii_string(rng, 128),
                pid: rng.next_u32(),
                created: (u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32()),
            },
            1 => SupervisorReply::Refused {
                reason: if rng.bool() { SupervisorRefusedReason::StaleVoyage } else { SupervisorRefusedReason::VersionSkew },
            },
            2 => SupervisorReply::Operation(random_supervisor_operation_state(rng)),
            _ => SupervisorReply::StatusOk {
                pid: rng.next_u32(),
                created: (u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32()),
                voyage: if rng.bool() { Some(random_nonempty_ascii_string(rng, 128)) } else { None },
                leg: if rng.bool() { Some((u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32())) } else { None },
                phase: match rng.below(5) {
                    0 => SupervisorPhase::Starting,
                    1 => SupervisorPhase::Ready,
                    2 => SupervisorPhase::Ending,
                    3 => SupervisorPhase::EndedNoRespawn,
                    _ => SupervisorPhase::Terminal,
                },
            },
        };
        GeneratedFrame::SupervisorReply(f)
    }
}

/// Which lane a generated sequence rides — every frame in one call comes
/// from the SAME lane, matching the real protocol's own "latched by the
/// first frame's magic" rule (a mixed-lane sequence is exactly what
/// `LaneMismatch` exists to reject, a DIFFERENT, already-covered test).
#[derive(Debug, Clone, Copy)]
enum Lane {
    Mgmt,
    Attach,
    Supervisor,
}

fn random_lane(rng: &mut Rng) -> Lane {
    match rng.below(3) {
        0 => Lane::Mgmt,
        1 => Lane::Attach,
        _ => Lane::Supervisor,
    }
}

fn random_sequence(rng: &mut Rng, lane: Lane, count: usize) -> Vec<GeneratedFrame> {
    (0..count)
        .map(|_| match lane {
            Lane::Mgmt => random_mgmt_frame(rng),
            Lane::Attach => random_attach_frame(rng),
            Lane::Supervisor => random_supervisor_frame(rng),
        })
        .collect()
}

fn decode_all_at_once(bytes: &[u8]) -> Vec<DecodedFrame> {
    let mut splitter = FrameSplitter::new();
    let (frames, err) = splitter.feed(bytes);
    assert_eq!(err, None, "a valid stream must decode without error");
    frames
}

fn decode_in_chunks(bytes: &[u8], chunk_bounds: &[usize]) -> Vec<DecodedFrame> {
    let mut splitter = FrameSplitter::new();
    let mut out = Vec::new();
    let mut start = 0;
    for &end in chunk_bounds {
        let (frames, err) = splitter.feed(&bytes[start..end]);
        assert_eq!(err, None, "a valid stream must decode without error");
        out.extend(frames);
        start = end;
    }
    let (frames, err) = splitter.feed(&bytes[start..]);
    assert_eq!(err, None, "a valid stream must decode without error");
    out.extend(frames);
    out
}

/// Feeds every chunk in order, accumulating frames across calls and
/// capturing the FIRST error observed (later calls after a failure only
/// ever repeat it, per the failed-state latch) -- the outcome a caller
/// actually cares about is this cumulative pair, not any one call's
/// partial answer.
fn accumulate_feed(chunks: &[&[u8]]) -> (Vec<DecodedFrame>, Option<WireError>) {
    let mut splitter = FrameSplitter::new();
    let mut frames = Vec::new();
    let mut error = None;
    for chunk in chunks {
        let (f, e) = splitter.feed(chunk);
        frames.extend(f);
        if error.is_none() {
            error = e;
        }
    }
    (frames, error)
}

// ------------------------------------------------------------------
// 2. Splitter: every 2-way cut position, plus one-byte-at-a-time.
// ------------------------------------------------------------------

#[test]
fn splitter_decodes_identically_at_every_two_way_split_point() {
    let mut rng = Rng(0xC0FF_EE00_D15E_A5E1);
    let frames = random_sequence(&mut rng, Lane::Attach, 12);
    let bytes: Vec<u8> = frames.iter().flat_map(|f| f.encode()).collect();
    let expected = decode_all_at_once(&bytes);
    assert_eq!(expected.len(), frames.len());

    for split in 0..=bytes.len() {
        let got = decode_in_chunks(&bytes, &[split]);
        assert_eq!(
            got, expected,
            "mismatch splitting the attach-lane stream at byte {split} of {}",
            bytes.len()
        );
    }
}

#[test]
fn splitter_decodes_identically_at_every_two_way_split_point_mgmt_lane() {
    let mut rng = Rng(0x5EED_1234_5678_9ABC);
    let frames = random_sequence(&mut rng, Lane::Mgmt, 10);
    let bytes: Vec<u8> = frames.iter().flat_map(|f| f.encode()).collect();
    let expected = decode_all_at_once(&bytes);

    for split in 0..=bytes.len() {
        let got = decode_in_chunks(&bytes, &[split]);
        assert_eq!(
            got, expected,
            "mismatch splitting the mgmt-lane stream at byte {split} of {}",
            bytes.len()
        );
    }
}

#[test]
fn splitter_decodes_identically_fed_one_byte_at_a_time() {
    let mut rng = Rng(0x1111_2222_3333_4444);
    let frames = random_sequence(&mut rng, Lane::Attach, 15);
    let bytes: Vec<u8> = frames.iter().flat_map(|f| f.encode()).collect();
    let expected = decode_all_at_once(&bytes);

    let bounds: Vec<usize> = (1..bytes.len()).collect();
    let got = decode_in_chunks(&bytes, &bounds);
    assert_eq!(got, expected, "byte-at-a-time feed diverged from one-shot decode");
}

// ------------------------------------------------------------------
// should-fix 3: the same (frames, error) outcome at every split point,
// for a stream that DOES error partway through.
// ------------------------------------------------------------------

#[test]
fn splitter_reports_identical_frames_and_error_at_every_split_point() {
    // A few valid mgmt-lane frames, then one with an unknown tag on the
    // SAME lane (so the error is `UnknownTag`, never a lane mismatch).
    let mut rng = Rng(0x0FA1_1ED0_FA11_ED00);
    let good_frames = random_sequence(&mut rng, Lane::Mgmt, 5);
    let mut bytes: Vec<u8> = good_frames.iter().flat_map(|f| f.encode()).collect();
    let bad_body = vec![0x99u8];
    bytes.extend_from_slice(&MGMT_MAGIC);
    bytes.extend_from_slice(&(bad_body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&bad_body);

    let expected_frames: Vec<DecodedFrame> =
        good_frames.iter().map(GeneratedFrame::as_decoded).collect();

    for split in 0..=bytes.len() {
        let (frames, error) = accumulate_feed(&[&bytes[..split], &bytes[split..]]);
        assert_eq!(frames, expected_frames, "split at {split} of {}: frames diverged", bytes.len());
        assert_eq!(
            error,
            Some(WireError::UnknownTag(0x99)),
            "split at {split} of {}: error diverged",
            bytes.len()
        );
    }
}

// ------------------------------------------------------------------
// 7. Fuzz-lite.
// ------------------------------------------------------------------

#[test]
fn fuzz_random_valid_sequences_roundtrip_through_random_chunk_boundaries() {
    for seed in [
        0x0000_0000_0000_0001u64,
        0xdead_beef_cafe_babe,
        0x1234_5678_9abc_def0,
        0xffff_ffff_ffff_ffff,
        0x0bad_c0de_c0ff_ee11,
    ] {
        let mut rng = Rng(seed);
        for iteration in 0..40 {
            let lane = random_lane(&mut rng);
            let count = 1 + rng.below(10) as usize;
            let frames = random_sequence(&mut rng, lane, count);
            let bytes: Vec<u8> = frames.iter().flat_map(|f| f.encode()).collect();
            let expected: Vec<DecodedFrame> = frames.iter().map(GeneratedFrame::as_decoded).collect();

            // One-shot.
            let one_shot = decode_all_at_once(&bytes);
            assert_eq!(
                one_shot, expected,
                "seed {seed:#x} iteration {iteration}: one-shot decode did not match what was generated"
            );

            // Random chunk boundaries.
            let mut bounds: Vec<usize> = Vec::new();
            if !bytes.is_empty() {
                let cuts = rng.below(6);
                for _ in 0..cuts {
                    bounds.push(rng.below((bytes.len() + 1) as u32) as usize);
                }
            }
            bounds.sort_unstable();
            let chunked = decode_in_chunks(&bytes, &bounds);
            assert_eq!(
                chunked, expected,
                "seed {seed:#x} iteration {iteration}: chunked decode at bounds {bounds:?} \
                 diverged (stream length {})",
                bytes.len()
            );
        }
    }
}

#[test]
fn fuzz_random_mutations_of_valid_streams_never_panic() {
    for seed in [0x5117_5117_5117_5117u64, 0xabad_1dea_abad_1dea, 0x0102_0304_0506_0708] {
        let mut rng = Rng(seed);
        for iteration in 0..200 {
            let lane = random_lane(&mut rng);
            let count = 1 + rng.below(8) as usize;
            let frames = random_sequence(&mut rng, lane, count);
            let mut bytes: Vec<u8> = frames.iter().flat_map(|f| f.encode()).collect();
            if bytes.is_empty() {
                continue;
            }

            match rng.below(4) {
                0 => {
                    // Flip a random byte -- XOR 0xff can never be a no-op.
                    let idx = rng.below(bytes.len() as u32) as usize;
                    bytes[idx] ^= 0xff;
                }
                1 => {
                    // Truncate at a random point STRICTLY shorter than
                    // the original -- `below(bytes.len())` never returns
                    // `bytes.len()` itself, so this can never be a no-op.
                    let cut = rng.below(bytes.len() as u32) as usize;
                    bytes.truncate(cut);
                }
                2 => {
                    // Insert 1-8 bytes of random garbage at a random
                    // position -- never zero bytes.
                    let idx = rng.below((bytes.len() + 1) as u32) as usize;
                    let garbage: Vec<u8> =
                        (0..1 + rng.below(8)).map(|_| rng.below(256) as u8).collect();
                    bytes.splice(idx..idx, garbage);
                }
                _ => {
                    // Overwrite a run of 1-4 bytes with random bytes --
                    // never a zero-length (no-op) overwrite.
                    let start = rng.below(bytes.len() as u32) as usize;
                    let run = 1 + rng.below(4) as usize;
                    for b in bytes.iter_mut().skip(start).take(run) {
                        *b = rng.below(256) as u8;
                    }
                }
            }

            // Feed at a couple of random chunk boundaries -- the property
            // under test is "never panics", not any particular decode
            // result (a mutation may legally decode to an unrelated but
            // still well-formed frame, error out, or simply carry).
            let cut = rng.below((bytes.len() + 1) as u32) as usize;
            let bytes_for_panic_check = bytes.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut splitter = FrameSplitter::new();
                let (_, err) = splitter.feed(&bytes_for_panic_check[..cut]);
                if err.is_some() {
                    return;
                }
                let _ = splitter.feed(&bytes_for_panic_check[cut..]);
            }));
            assert!(
                outcome.is_ok(),
                "seed {seed:#x} iteration {iteration}: mutated stream panicked (cut at {cut} of {})",
                bytes_for_panic_check.len()
            );
        }
    }
}

// -----------------------------------------------------------------------
// supervisor-lane-v1 fixture (ADR 0045 decision 7): mirrors
// `tests/golden.rs:66-110`'s pattern -- an immutable committed file, one
// instance of EVERY top-level `SupervisorRequest`/`SupervisorReply`
// variant. Embedded via `include_bytes!`, not read at runtime: a
// committed fixture is immutable by construction, so this file carries
// no writer function for it -- a wire change is a NEW protocol version,
// proven by a SECOND fixture (a `-v2` file, hand-written or a one-off
// script), never a rewrite of this one. What this proves and what it
// does not: two processes agreeing on `SUPERVISOR_PROTO_V1` agree on
// these BYTES -- it cannot prove semantics, and command bytes feed
// durable journal digests (`wire.rs:1098`), so a lane bump that changes
// a command's encoding is ALSO a `journal::SCHEMA_VERSION` event. The
// NESTED variants this file leaves unpinned (`SupervisorOp::Reset`/
// `Stop`, every `SupervisorOperationState`, every `SupervisorPhase`,
// every `SupervisorRefusedReason`, `StatusOk`'s optional fields both
// present and absent) are covered by the coverage fixture below instead
// (Codex review finding, 2026-09-11: eight top-level variants alone
// leave `Stop`'s own tag, say, free to change without failing anything).
// -----------------------------------------------------------------------

/// Fixed field values, stable declaration order -- one instance of every
/// top-level `SupervisorRequest`/`SupervisorReply` variant.
fn supervisor_lane_v1_frames() -> Vec<Vec<u8>> {
    vec![
        encode_supervisor_request(&SupervisorRequest::Hello {
            proto: SUPERVISOR_PROTO_V1,
            build: "fixture-build".into(),
        })
        .unwrap(),
        encode_supervisor_request(&SupervisorRequest::Command {
            operation_id: "op-1".into(),
            op: SupervisorOp::EndRun {
                reason: "fixture".into(),
                voyage: "01900000-0000-7000-8000-000000000001".into(),
            },
        })
        .unwrap(),
        encode_supervisor_request(&SupervisorRequest::Status).unwrap(),
        encode_supervisor_request(&SupervisorRequest::Query { operation_id: "op-1".into() }).unwrap(),
        encode_supervisor_reply(&SupervisorReply::HelloOk {
            proto: SUPERVISOR_PROTO_V1,
            build: "fixture-build".into(),
            pid: 4242,
            created: 1_756_000_000,
        })
        .unwrap(),
        encode_supervisor_reply(&SupervisorReply::Refused { reason: SupervisorRefusedReason::VersionSkew }).unwrap(),
        encode_supervisor_reply(&SupervisorReply::Operation(SupervisorOperationState::Accepted)).unwrap(),
        encode_supervisor_reply(&SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: Some(7),
            phase: SupervisorPhase::Ready,
        })
        .unwrap(),
    ]
}

/// Pins the embedded `supervisor-lane-v1.bin` against a fresh encoding of
/// [`supervisor_lane_v1_frames`] -- see this section's own header doc for
/// what this proves and what it does not.
#[test]
fn supervisor_lane_v1_bytes_are_pinned() {
    let generated: Vec<u8> = supervisor_lane_v1_frames().into_iter().flatten().collect();
    const COMMITTED: &[u8] = include_bytes!("fixtures/supervisor-lane-v1.bin");
    assert_eq!(
        COMMITTED, generated.as_slice(),
        "supervisor lane v1 wire bytes changed -- this is a NEW PROTOCOL VERSION \
         (ADR 0045 decision 7), not test drift: add a -v2 fixture instead of editing this one"
    );
}

// -----------------------------------------------------------------------
// supervisor-lane-v1-coverage fixture: the SAME immutability contract as
// the fixture above, in a SECOND embedded file -- never an edit to
// `supervisor-lane-v1.bin`, which stays byte-identical. Pins every
// NESTED variant and every optional-field absent/present form the
// top-level fixture leaves uncovered: `SupervisorOp::Reset` (both with
// and without its optional `voyage`) and `Stop`; all seven
// `SupervisorOperationState` variants; every `SupervisorPhase`; every
// `SupervisorRefusedReason`, both as a top-level `Refused` and inside
// `Operation(Refused)`; and `StatusOk`'s `voyage`/`leg` in their absent,
// mixed, and fully-present forms. Proven TWO ways, not just round-
// tripped: the concatenated bytes are pinned against the committed file
// exactly like the fixture above, AND those same committed bytes are
// independently DECODED through a fresh `FrameSplitter` and compared
// frame-for-frame against the expected `DecodedFrame` values -- a
// decoder bug that happens to still produce matching encoder output
// cannot hide behind the byte pin alone.
// -----------------------------------------------------------------------

/// One `(encoded bytes, expected decoded frame)` pair per nested variant
/// / optional-field form this fixture exists to cover.
fn supervisor_lane_v1_coverage_frames() -> Vec<(Vec<u8>, DecodedFrame)> {
    let req = |r: SupervisorRequest| {
        let bytes = encode_supervisor_request(&r).unwrap();
        (bytes, DecodedFrame::SupervisorRequest(r))
    };
    let rep = |r: SupervisorReply| {
        let bytes = encode_supervisor_reply(&r).unwrap();
        (bytes, DecodedFrame::SupervisorReply(r))
    };
    vec![
        req(SupervisorRequest::Command {
            operation_id: "op-2".into(),
            op: SupervisorOp::Reset { voyage: Some("01900000-0000-7000-8000-000000000002".into()) },
        }),
        req(SupervisorRequest::Command { operation_id: "op-3".into(), op: SupervisorOp::Reset { voyage: None } }),
        req(SupervisorRequest::Command { operation_id: "op-4".into(), op: SupervisorOp::Stop }),
        rep(SupervisorReply::Operation(SupervisorOperationState::RecordClosed)),
        rep(SupervisorReply::Operation(SupervisorOperationState::RecordVerified)),
        rep(SupervisorReply::Operation(SupervisorOperationState::ResetDone {
            new_voyage: "01900000-0000-7000-8000-000000000003".into(),
        })),
        rep(SupervisorReply::Operation(SupervisorOperationState::Stopping)),
        rep(SupervisorReply::Operation(SupervisorOperationState::Failed { detail: "fixture failure".into() })),
        rep(SupervisorReply::Operation(SupervisorOperationState::Refused {
            reason: SupervisorRefusedReason::StaleVoyage,
        })),
        rep(SupervisorReply::Operation(SupervisorOperationState::Refused {
            reason: SupervisorRefusedReason::IdConflict,
        })),
        rep(SupervisorReply::Operation(SupervisorOperationState::UnknownOperation)),
        rep(SupervisorReply::Refused { reason: SupervisorRefusedReason::StaleVoyage }),
        rep(SupervisorReply::Refused { reason: SupervisorRefusedReason::IdConflict }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: None,
            leg: None,
            phase: SupervisorPhase::Starting,
        }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: None,
            phase: SupervisorPhase::Ready,
        }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: Some(7),
            phase: SupervisorPhase::Ending,
        }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: Some(7),
            phase: SupervisorPhase::EndedNoRespawn,
        }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: Some(7),
            phase: SupervisorPhase::Terminal,
        }),
    ]
}

/// Pins the embedded `supervisor-lane-v1-coverage.bin` against a fresh
/// encoding of [`supervisor_lane_v1_coverage_frames`], THEN decodes the
/// committed bytes back through a fresh [`FrameSplitter`] and compares
/// the result frame-for-frame against what each entry expects -- see
/// this section's own header doc for why both checks matter.
#[test]
fn supervisor_lane_v1_coverage_bytes_are_pinned_and_decode_back_identically() {
    let pairs = supervisor_lane_v1_coverage_frames();
    let generated: Vec<u8> = pairs.iter().flat_map(|(bytes, _)| bytes.clone()).collect();
    const COMMITTED: &[u8] = include_bytes!("fixtures/supervisor-lane-v1-coverage.bin");
    assert_eq!(
        COMMITTED, generated.as_slice(),
        "supervisor lane v1 coverage wire bytes changed -- this is a NEW PROTOCOL VERSION \
         (ADR 0045 decision 7), not test drift: add a -v2 fixture instead of editing this one"
    );

    let mut splitter = FrameSplitter::new();
    let (frames, err) = splitter.feed(COMMITTED);
    assert!(err.is_none(), "the committed coverage fixture must decode cleanly: {err:?}");
    let expected: Vec<DecodedFrame> = pairs.into_iter().map(|(_, frame)| frame).collect();
    assert_eq!(frames, expected, "the committed coverage fixture must decode back to exactly these frames");
}
