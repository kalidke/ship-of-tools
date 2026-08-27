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
//! multi-frame stream, and fuzzing.

use sot_log::wire::{
    encode_attach_client, encode_attach_server, encode_mgmt_reply, encode_mgmt_request,
    AttachClient, AttachRefusedReason, AttachServer, DecodedFrame, FrameSplitter, MgmtReply,
    MgmtRequest, ResizeRefusedReason, Survival, TakeRefusedReason,
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

/// One frame, tagged by which of the four `encode_*` functions it rides.
#[derive(Debug, Clone)]
enum GeneratedFrame {
    MgmtRequest(MgmtRequest),
    MgmtReply(MgmtReply),
    AttachClient(AttachClient),
    AttachServer(AttachServer),
}

impl GeneratedFrame {
    fn encode(&self) -> Vec<u8> {
        match self {
            Self::MgmtRequest(f) => encode_mgmt_request(f).expect("generator stays in-bounds"),
            Self::MgmtReply(f) => encode_mgmt_reply(f).expect("generator stays in-bounds"),
            Self::AttachClient(f) => encode_attach_client(f).expect("generator stays in-bounds"),
            Self::AttachServer(f) => encode_attach_server(f).expect("generator stays in-bounds"),
        }
    }

    fn as_decoded(&self) -> DecodedFrame {
        match self {
            Self::MgmtRequest(f) => DecodedFrame::MgmtRequest(f.clone()),
            Self::MgmtReply(f) => DecodedFrame::MgmtReply(f.clone()),
            Self::AttachClient(f) => DecodedFrame::AttachClient(f.clone()),
            Self::AttachServer(f) => DecodedFrame::AttachServer(f.clone()),
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
    if rng.bool() {
        let f = match rng.below(6) {
            0 => AttachClient::Hello {
                proto: rng.next_u32(),
            },
            1 => AttachClient::Attach {
                controller_id: random_ascii_string(rng, 128),
            },
            2 => AttachClient::Take {
                controller_id: random_ascii_string(rng, 128),
            },
            3 => AttachClient::Input {
                controller_id: random_ascii_string(rng, 128),
                take_epoch: (u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32()),
                idem_key: random_idem_key(rng),
                payload: random_bytes(rng, 200), // small on purpose -- bounds are tested separately
            },
            4 => AttachClient::Resize {
                cols: rng.below(513) as u16,
                rows: rng.below(257) as u16,
            },
            _ => AttachClient::Keepalive {
                nonce: (u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32()),
            },
        };
        GeneratedFrame::AttachClient(f)
    } else {
        let f = match rng.below(13) {
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
            11 => AttachServer::ResizeRefused {
                reason: if rng.bool() {
                    ResizeRefusedReason::OutOfBudget
                } else {
                    ResizeRefusedReason::NotDriver
                },
            },
            _ => AttachServer::Keepalive {
                nonce: (u64::from(rng.next_u32()) << 32) | u64::from(rng.next_u32()),
            },
        };
        GeneratedFrame::AttachServer(f)
    }
}

fn random_sequence(rng: &mut Rng, mgmt_lane: bool, count: usize) -> Vec<GeneratedFrame> {
    (0..count)
        .map(|_| {
            if mgmt_lane {
                random_mgmt_frame(rng)
            } else {
                random_attach_frame(rng)
            }
        })
        .collect()
}

fn decode_all_at_once(bytes: &[u8]) -> Vec<DecodedFrame> {
    let mut splitter = FrameSplitter::new();
    splitter.feed(bytes).expect("a valid stream must decode")
}

fn decode_in_chunks(bytes: &[u8], chunk_bounds: &[usize]) -> Vec<DecodedFrame> {
    let mut splitter = FrameSplitter::new();
    let mut out = Vec::new();
    let mut start = 0;
    for &end in chunk_bounds {
        out.extend(splitter.feed(&bytes[start..end]).expect("a valid stream must decode"));
        start = end;
    }
    out.extend(splitter.feed(&bytes[start..]).expect("a valid stream must decode"));
    out
}

// ------------------------------------------------------------------
// 2. Splitter: every 2-way cut position, plus one-byte-at-a-time.
// ------------------------------------------------------------------

#[test]
fn splitter_decodes_identically_at_every_two_way_split_point() {
    let mut rng = Rng(0xC0FF_EE00_D15E_A5E1);
    let frames = random_sequence(&mut rng, false, 12);
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
    let frames = random_sequence(&mut rng, true, 10);
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
    let frames = random_sequence(&mut rng, false, 15);
    let bytes: Vec<u8> = frames.iter().flat_map(|f| f.encode()).collect();
    let expected = decode_all_at_once(&bytes);

    let bounds: Vec<usize> = (1..bytes.len()).collect();
    let got = decode_in_chunks(&bytes, &bounds);
    assert_eq!(got, expected, "byte-at-a-time feed diverged from one-shot decode");
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
            let mgmt_lane = rng.bool();
            let count = 1 + rng.below(10) as usize;
            let frames = random_sequence(&mut rng, mgmt_lane, count);
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
            let mgmt_lane = rng.bool();
            let count = 1 + rng.below(8) as usize;
            let frames = random_sequence(&mut rng, mgmt_lane, count);
            let mut bytes: Vec<u8> = frames.iter().flat_map(|f| f.encode()).collect();
            if bytes.is_empty() {
                continue;
            }

            match rng.below(4) {
                0 => {
                    // Flip a random byte.
                    let idx = rng.below(bytes.len() as u32) as usize;
                    bytes[idx] ^= 0xff;
                }
                1 => {
                    // Truncate at a random point.
                    let cut = rng.below((bytes.len() + 1) as u32) as usize;
                    bytes.truncate(cut);
                }
                2 => {
                    // Insert random garbage at a random position.
                    let idx = rng.below((bytes.len() + 1) as u32) as usize;
                    let garbage: Vec<u8> = (0..rng.below(8)).map(|_| rng.below(256) as u8).collect();
                    bytes.splice(idx..idx, garbage);
                }
                _ => {
                    // Overwrite a random run with random bytes.
                    let start = rng.below(bytes.len() as u32) as usize;
                    let run = rng.below(5) as usize;
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
                let first = splitter.feed(&bytes_for_panic_check[..cut]);
                if first.is_err() {
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
