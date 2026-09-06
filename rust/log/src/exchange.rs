//! A pipe lane's own post-SID identity exchange (ADR 0041 Lifecycle "The
//! challenge", steps 4-5): what request bytes to send once, and how to
//! decode the reply into a `(pid, created)` pair or a foreign/incomplete
//! verdict. Portable — the wire codec this depends on
//! ([`wire::FrameSplitter`]) already builds and is tested on every
//! platform, so this module's frame-loop precedence logic is exercised by
//! REAL executed tests everywhere, not merely compile-checked on
//! Windows. `challenge::exchange_identity` (portable — steps 1-3, the
//! genuine OS calls, are each platform's own job) is the ONLY caller of
//! either trait method, and is called only after step 3 (identity
//! equality) has already succeeded — implementing [`IdentityExchange`]
//! cannot skip or reorder the OS authentication steps, because this
//! trait never sees the connection until they are done.

use crate::wire::{self, DecodedFrame, MgmtReply, MgmtRequest, SupervisorReply, SupervisorRequest};

/// What one lane's post-SID exchange concluded, fed one chunk of newly
/// read bytes at a time.
pub enum ExchangeDecode {
    /// Not enough bytes yet for this lane's own framing to decide
    /// anything — keep reading.
    Incomplete,
    /// Exactly one well-formed reply, naming the peer's pid and creation
    /// time.
    Identity { pid: u32, created: u64 },
    /// A well-formed WRONG answer, undecodable bytes, a frame over this
    /// lane's own size cap, or MORE than one complete reply/frame in what
    /// should have been a single round trip. An unproven server — this
    /// lane's own framing is the only thing that can tell "one clean
    /// reply" from "trailing protocol corruption", which is exactly why
    /// this decision lives in the lane, not in `challenge::exchange_identity`.
    Foreign,
}

/// A pipe lane's own post-SID identity exchange.
pub trait IdentityExchange {
    /// This lane's one-shot identity-yielding request, written once as
    /// the first content on the connection after SID equality.
    fn encode_request(&self) -> Vec<u8>;
    /// Consume newly-read bytes (appended to whatever this lane's own
    /// decoder already holds from earlier calls) and report what they now
    /// decode to. `&mut self`: a lane's own decoder (e.g. a
    /// [`wire::FrameSplitter`]) is itself stateful across calls.
    fn feed(&mut self, bytes: &[u8]) -> ExchangeDecode;
}

/// The voyage mgmt lane's `IdentityExchange`: `status` request,
/// `status_ok` reply — today's only lane. Error takes precedence over a
/// decoded frame, and exactly one complete reply is accepted per
/// exchange: a `StatusOk` bundled with trailing malformed bytes, or two
/// complete replies in one read, is `Foreign`, never the first frame
/// alone (ADR 0041 U0 round-1 finding 5).
///
/// TERMINAL after one conclusive answer (round-2 finding 1): `done`
/// latches on the FIRST `Identity` or `Foreign` this exchange ever
/// reaches, and every later `feed` call — regardless of what bytes it
/// carries, including none of its own — returns `Foreign` without
/// touching the splitter again. This closes two leaks the round-1
/// version had: a second complete `status_ok` arriving in a LATER `feed`
/// call (the splitter alone has no memory that this exchange already
/// answered) and a single trailing partial-header byte riding along with
/// a valid reply in the SAME call (`frames.len() == 1` looks identical
/// whether or not the splitter is also carrying leftover bytes, so
/// [`wire::FrameSplitter::has_pending_bytes`] is the one way to tell them
/// apart).
pub struct VoyageMgmtExchange {
    splitter: wire::FrameSplitter,
    done: bool,
}

impl Default for VoyageMgmtExchange {
    fn default() -> Self {
        Self { splitter: wire::FrameSplitter::new(), done: false }
    }
}

impl IdentityExchange for VoyageMgmtExchange {
    fn encode_request(&self) -> Vec<u8> {
        wire::encode_mgmt_request(&MgmtRequest::Status)
            .expect("MgmtRequest::Status has no fields; encoding cannot fail")
    }

    fn feed(&mut self, bytes: &[u8]) -> ExchangeDecode {
        if self.done {
            // Anything after a conclusive answer is corruption, never a
            // "next" reply — this lane's protocol is exactly one round
            // trip.
            return ExchangeDecode::Foreign;
        }
        let (frames, err) = self.splitter.feed(bytes);
        // Finding 5: error takes precedence over any frame decoded in the
        // SAME call, and more than one complete frame is corruption too —
        // both checked BEFORE ever looking at frame contents.
        if err.is_some() {
            self.done = true;
            return ExchangeDecode::Foreign;
        }
        match frames.len() {
            0 => ExchangeDecode::Incomplete,
            1 => {
                self.done = true; // conclusive either way, from here on
                if self.splitter.has_pending_bytes() {
                    // A trailing byte (or more) rode along with the ONE
                    // reply this exchange is entitled to — corruption,
                    // never silently carried past.
                    return ExchangeDecode::Foreign;
                }
                match &frames[0] {
                    DecodedFrame::MgmtReply(MgmtReply::StatusOk { pid, created, .. }) => {
                        ExchangeDecode::Identity { pid: *pid, created: *created }
                    }
                    _ => ExchangeDecode::Foreign, // well-formed, wrong opcode
                }
            }
            _ => {
                self.done = true;
                ExchangeDecode::Foreign // two-or-more complete replies in one read
            }
        }
    }
}

/// This build's own identity, carried in the supervisor lane's `hello`
/// (ADR 0041 Lifecycle "Build boundary"): "a build identity is
/// compatibility data, not a credential." A bare crate version is NOT
/// enough — two different commits can share one `Cargo.toml` version
/// between releases, and "file replacement is not process replacement"
/// (an old, still-live supervisor answering under a newly-replaced
/// binary must not be treated as compatible with a client built against
/// the new one) needs a value that actually changes commit-to-commit.
/// `build.rs` stamps `SOT_LOG_BUILD_SHA` from the FULL `git rev-parse
/// HEAD`, `-dirty`-suffixed over an uncommitted tree — the same
/// git-sha-capture pattern `rust/protocol/build.rs` uses for
/// `app_version` (copied rather than shared: `sot-log` must not depend on
/// `sot-protocol`, see `build.rs`'s own doc comment). `build.rs` itself
/// FAILS the build rather than emitting an empty/ambiguous value when no
/// git repository is found (Codex review round 2, finding M9: silently
/// falling back to the bare package version here made two different
/// commits sharing one pre-release version indistinguishable — exactly
/// what this identity exists to prevent) — this constant can therefore
/// simply trust the env var is always a real, nonempty identity, with no
/// runtime fallback branch of its own. A stronger, executable-hash-based
/// identity is explicitly out of scope ("Executable attestation —
/// excluded by the threat model, not deferred").
pub const SUPERVISOR_LANE_BUILD_ID: &str = env!("SOT_LOG_BUILD_SHA");

/// The supervisor lane's own `IdentityExchange` (ADR 0041 Lifecycle "The
/// challenge", steps 4-5): `hello {proto, build}` request, `hello_ok`
/// reply — this lane's identity-yielding exchange AND its build-boundary
/// check in one round trip, per [`wire::SupervisorRequest::Hello`]'s own
/// doc ("doubles as the same-connection challenge's own steps 4-5").
/// `hello_refused {version_skew}`, or anything else that is not a clean,
/// solitary `hello_ok`, is `Foreign` — an unproven server, exactly like
/// [`VoyageMgmtExchange`]'s own handling of a wrong reply. Structurally
/// identical to `VoyageMgmtExchange` (terminal after one conclusive
/// answer, a trailing byte alongside a valid reply is corruption, two
/// complete replies in one read is corruption) because it is the SAME
/// exchange contract over a different lane.
pub struct SupervisorLaneExchange {
    /// Bounded at construction (see [`Self::new`]) so [`Self::encode_request`]'s
    /// `expect` — this trait has no `Result` to return one through — can
    /// never actually fail.
    build: String,
    splitter: wire::FrameSplitter,
    done: bool,
}

impl SupervisorLaneExchange {
    /// `build` is truncated to `wire::MAX_SUPERVISOR_STRING_LEN` bytes, on
    /// a UTF-8 boundary, if it somehow exceeds it — [`SUPERVISOR_LANE_BUILD_ID`]
    /// never will, but this constructor takes an owned `String` rather
    /// than that one constant so a test can exercise a foreign/oversized
    /// build id without a second code path.
    pub fn new(build: impl Into<String>) -> Self {
        let mut build = build.into();
        if build.len() > wire::MAX_SUPERVISOR_STRING_LEN {
            // Find the boundary BEFORE truncating (Codex review round 2,
            // finding M4): `String::truncate` itself panics if the cut
            // point splits a multi-byte codepoint — the classic
            // "truncate-then-fix" ordering never reaches its own repair
            // loop when that happens. Both existing tests used repeated
            // `é`, for which byte 128 happens to land on a boundary,
            // which is exactly how this went unnoticed.
            let mut cut = wire::MAX_SUPERVISOR_STRING_LEN;
            while !build.is_char_boundary(cut) {
                cut -= 1;
            }
            build.truncate(cut);
        }
        Self { build, splitter: wire::FrameSplitter::new(), done: false }
    }
}

impl IdentityExchange for SupervisorLaneExchange {
    fn encode_request(&self) -> Vec<u8> {
        wire::encode_supervisor_request(&SupervisorRequest::Hello {
            proto: wire::SUPERVISOR_PROTO_V1,
            build: self.build.clone(),
        })
        .expect("build is bounded at construction; encoding cannot fail")
    }

    fn feed(&mut self, bytes: &[u8]) -> ExchangeDecode {
        if self.done {
            return ExchangeDecode::Foreign;
        }
        let (frames, err) = self.splitter.feed(bytes);
        if err.is_some() {
            self.done = true;
            return ExchangeDecode::Foreign;
        }
        match frames.len() {
            0 => ExchangeDecode::Incomplete,
            1 => {
                self.done = true;
                if self.splitter.has_pending_bytes() {
                    return ExchangeDecode::Foreign;
                }
                match &frames[0] {
                    DecodedFrame::SupervisorReply(SupervisorReply::HelloOk {
                        proto,
                        build,
                        pid,
                        created,
                    }) => {
                        // The boundary is mutual (ADR 0041 Lifecycle "Build
                        // boundary"): this lane's request already carries
                        // OUR proto/build for the server to check; a
                        // `hello_ok` that doesn't echo them back is not
                        // proof of anything either — it could be a stale
                        // peer's reply to a DIFFERENT client's `hello`
                        // (or corruption) surviving just long enough to
                        // parse. Treat a mismatch exactly like any other
                        // wrong reply: `Foreign`, not `Identity`.
                        if *proto == wire::SUPERVISOR_PROTO_V1 && *build == self.build {
                            ExchangeDecode::Identity { pid: *pid, created: *created }
                        } else {
                            ExchangeDecode::Foreign
                        }
                    }
                    _ => ExchangeDecode::Foreign, // hello_refused, or any other well-formed reply
                }
            }
            _ => {
                self.done = true;
                ExchangeDecode::Foreign
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_ok_bytes(pid: u32, created: u64) -> Vec<u8> {
        wire::encode_mgmt_reply(&MgmtReply::StatusOk { pid, created, survival: wire::Survival::Normal }).unwrap()
    }

    #[test]
    fn incomplete_on_a_partial_frame() {
        let mut ex = VoyageMgmtExchange::default();
        let full = status_ok_bytes(1, 2);
        assert!(matches!(ex.feed(&full[..full.len() - 1]), ExchangeDecode::Incomplete));
    }

    #[test]
    fn decodes_exactly_one_clean_status_ok() {
        let mut ex = VoyageMgmtExchange::default();
        let full = status_ok_bytes(123, 456);
        match ex.feed(&full) {
            ExchangeDecode::Identity { pid, created } => {
                assert_eq!(pid, 123);
                assert_eq!(created, 456);
            }
            ExchangeDecode::Foreign => panic!("expected Identity, got Foreign"),
            ExchangeDecode::Incomplete => panic!("expected Identity, got Incomplete"),
        }
    }

    #[test]
    fn two_complete_replies_in_one_feed_is_foreign() {
        let mut ex = VoyageMgmtExchange::default();
        let mut bytes = status_ok_bytes(1, 2);
        bytes.extend(status_ok_bytes(1, 2));
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn status_ok_followed_by_malformed_bytes_is_foreign_not_proven() {
        // A frame-plus-error in the SAME feed call: the good StatusOk
        // must not mask the trailing corruption (finding 5).
        let mut ex = VoyageMgmtExchange::default();
        let mut bytes = status_ok_bytes(1, 2);
        bytes.extend([0xffu8; 16]); // an unknown magic -- UnknownMagic
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn malformed_first_frame_alone_is_foreign() {
        let mut ex = VoyageMgmtExchange::default();
        assert!(matches!(ex.feed(&[0xff; 16]), ExchangeDecode::Foreign));
    }

    #[test]
    fn oversized_first_frame_is_foreign() {
        let mut ex = VoyageMgmtExchange::default();
        // SOM0 magic + a length field announcing far more than MAX_BODY_LEN.
        let mut bytes = b"SOM0".to_vec();
        bytes.extend(u32::MAX.to_le_bytes());
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    /// Round-2 finding 1: the round-1 version of this test fed the "bad
    /// second" bytes to a FRESH exchange, which proves nothing about
    /// whether THIS SAME instance would still accept them after already
    /// answering once -- Codex reproduced exactly that gap (a second
    /// complete `status_ok` fed to the SAME instance in a later `feed`
    /// call returned `Identity` again). Rewritten to drive one instance
    /// throughout: a good reply, then a second complete reply arriving
    /// in a LATER `feed` call, must be `Foreign`.
    #[test]
    fn good_first_reply_then_a_later_complete_reply_on_the_same_instance_is_foreign() {
        let mut ex = VoyageMgmtExchange::default();
        assert!(matches!(ex.feed(&status_ok_bytes(1, 2)), ExchangeDecode::Identity { pid: 1, created: 2 }));
        // A second, otherwise-perfectly-well-formed reply, fed to the
        // SAME already-answered instance in a separate call.
        assert!(matches!(ex.feed(&status_ok_bytes(3, 4)), ExchangeDecode::Foreign));
    }

    /// Round-2 finding 1 (the other reproduced leak): a single trailing
    /// byte riding along with a valid reply in the SAME `feed` call —
    /// not even enough to determine a magic, so the OLD code's
    /// `frames.len() == 1` check alone could not distinguish this from
    /// "nothing left over" and returned `Identity`.
    #[test]
    fn status_ok_plus_one_trailing_partial_header_byte_is_foreign() {
        let mut ex = VoyageMgmtExchange::default();
        let mut bytes = status_ok_bytes(1, 2);
        bytes.push(0xAB); // a single byte: not even a full header's worth
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    /// Round-2 finding 1: once terminal, ANY further byte in a LATER
    /// call is Foreign, even bytes that on their own would be
    /// `Incomplete` for a fresh exchange (e.g. a lone partial byte).
    #[test]
    fn any_byte_at_all_after_a_conclusive_answer_is_foreign() {
        let mut ex = VoyageMgmtExchange::default();
        assert!(matches!(ex.feed(&status_ok_bytes(1, 2)), ExchangeDecode::Identity { .. }));
        assert!(matches!(ex.feed(&[0xAB]), ExchangeDecode::Foreign));
    }

    #[test]
    fn encode_request_is_the_pinned_status_wire_bytes() {
        let ex = VoyageMgmtExchange::default();
        assert_eq!(ex.encode_request(), wire::encode_mgmt_request(&MgmtRequest::Status).unwrap());
    }

    // --- SupervisorLaneExchange ---

    // Matches what `SupervisorLaneExchange::new("b")` sends, so the
    // "clean" tests below exercise the real mutual check (matching
    // proto+build) rather than accidentally skipping it.
    fn hello_ok_bytes(pid: u32, created: u64) -> Vec<u8> {
        hello_ok_bytes_with(wire::SUPERVISOR_PROTO_V1, "b", pid, created)
    }

    fn hello_ok_bytes_with(proto: u32, build: &str, pid: u32, created: u64) -> Vec<u8> {
        wire::encode_supervisor_reply(&SupervisorReply::HelloOk {
            proto,
            build: build.into(),
            pid,
            created,
        })
        .unwrap()
    }

    #[test]
    fn supervisor_encode_request_is_hello_with_this_build_id() {
        let ex = SupervisorLaneExchange::new("my-build");
        assert_eq!(
            ex.encode_request(),
            wire::encode_supervisor_request(&SupervisorRequest::Hello {
                proto: wire::SUPERVISOR_PROTO_V1,
                build: "my-build".into(),
            })
            .unwrap()
        );
    }

    #[test]
    fn supervisor_oversized_build_is_truncated_on_a_char_boundary_not_rejected() {
        // Multi-byte UTF-8 right at the truncation point: naive byte
        // slicing would split a codepoint and panic on `String::truncate`.
        let build: String = "é".repeat(wire::MAX_SUPERVISOR_STRING_LEN); // 2 bytes each
        let ex = SupervisorLaneExchange::new(build);
        let encoded = ex.encode_request(); // must not panic
        assert!(!encoded.is_empty());
    }

    /// Codex review round 2, finding M4: the ABOVE test's own `é` (2 bytes
    /// each) happens to land byte 128 exactly on a boundary (128 is even),
    /// which is precisely how the real bug — truncating to 128 BEFORE
    /// finding a boundary, which panics if byte 128 lands mid-codepoint —
    /// went unnoticed. A 3-byte codepoint repeated enough times to exceed
    /// 128 bytes puts byte 128 strictly INSIDE a character (126..129), so
    /// this exercises the actual straddle.
    #[test]
    fn supervisor_oversized_build_with_a_char_straddling_byte_128_does_not_panic() {
        let build: String = "€".repeat(50); // 3 bytes each = 150 bytes; byte 128 splits char 42
        assert!(!build.is_char_boundary(wire::MAX_SUPERVISOR_STRING_LEN), "test setup must actually straddle byte 128");
        let ex = SupervisorLaneExchange::new(build);
        let encoded = ex.encode_request(); // must not panic
        assert!(!encoded.is_empty());
    }

    #[test]
    fn supervisor_decodes_exactly_one_clean_hello_ok() {
        let mut ex = SupervisorLaneExchange::new("b");
        match ex.feed(&hello_ok_bytes(123, 456)) {
            ExchangeDecode::Identity { pid, created } => {
                assert_eq!(pid, 123);
                assert_eq!(created, 456);
            }
            ExchangeDecode::Foreign => panic!("expected Identity, got Foreign"),
            ExchangeDecode::Incomplete => panic!("expected Identity, got Incomplete"),
        }
    }

    #[test]
    fn supervisor_hello_refused_is_foreign() {
        let mut ex = SupervisorLaneExchange::new("b");
        let bytes = wire::encode_supervisor_reply(&SupervisorReply::Refused {
            reason: wire::SupervisorRefusedReason::VersionSkew,
        })
        .unwrap();
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn supervisor_two_complete_replies_in_one_feed_is_foreign() {
        let mut ex = SupervisorLaneExchange::new("b");
        let mut bytes = hello_ok_bytes(1, 2);
        bytes.extend(hello_ok_bytes(1, 2));
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn supervisor_hello_ok_plus_trailing_byte_is_foreign() {
        let mut ex = SupervisorLaneExchange::new("b");
        let mut bytes = hello_ok_bytes(1, 2);
        bytes.push(0xAB);
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn supervisor_any_byte_after_a_conclusive_answer_is_foreign() {
        let mut ex = SupervisorLaneExchange::new("b");
        assert!(matches!(ex.feed(&hello_ok_bytes(1, 2)), ExchangeDecode::Identity { .. }));
        assert!(matches!(ex.feed(&[0xAB]), ExchangeDecode::Foreign));
    }

    #[test]
    fn supervisor_incomplete_on_a_partial_frame() {
        let mut ex = SupervisorLaneExchange::new("b");
        let full = hello_ok_bytes(1, 2);
        assert!(matches!(ex.feed(&full[..full.len() - 1]), ExchangeDecode::Incomplete));
    }

    // The boundary is mutual (Codex review round 1, finding 7): a
    // `hello_ok` that doesn't echo THIS client's own proto/build back is
    // not proof of anything, even though it decodes cleanly — it could be
    // a stale reply to someone else's `hello`, or a server on a different
    // build replying without ever checking what it received.

    #[test]
    fn supervisor_hello_ok_with_wrong_build_is_foreign_not_identity() {
        let mut ex = SupervisorLaneExchange::new("b");
        let bytes = hello_ok_bytes_with(wire::SUPERVISOR_PROTO_V1, "different-build", 1, 2);
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn supervisor_hello_ok_with_wrong_proto_is_foreign_not_identity() {
        let mut ex = SupervisorLaneExchange::new("b");
        let bytes = hello_ok_bytes_with(wire::SUPERVISOR_PROTO_V1 + 1, "b", 1, 2);
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn supervisor_hello_ok_matching_this_build_id_constant_is_identity() {
        // Not just a fixture string: a `hello_ok` echoing the crate's
        // OWN real build id (what a genuine same-build peer would send)
        // must still be accepted.
        let mut ex = SupervisorLaneExchange::new(SUPERVISOR_LANE_BUILD_ID);
        let bytes = hello_ok_bytes_with(wire::SUPERVISOR_PROTO_V1, SUPERVISOR_LANE_BUILD_ID, 9, 10);
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Identity { pid: 9, created: 10 }));
    }
}
