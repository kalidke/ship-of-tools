//! The capsule lanes' own post-SID identity exchanges (ADR 0041 Lifecycle "The challenge", steps 4-5): what request bytes
//! to send once on the voyage mgmt lane and the supervisor lane, and how to decode each reply into a `(pid, created)`
//! pair or a foreign verdict. They implement platform's `IdentityExchange`, which the challenge drives only after the
//! OS steps have matched.

use crate::lane::wire::{self, DecodedFrame, MgmtReply, MgmtRequest, SupervisorReply, SupervisorRequest};
use sot_platform::identity::exchange::{ExchangeDecode, IdentityExchange};

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

/// This build's own identity: an informational build stamp carried in
/// the supervisor lane's `hello`/`hello_ok` — never compared (ADR 0045
/// decision 7 retires the "Build boundary" gate this constant used to
/// enforce; the lane gate is the `proto` integer alone, see
/// [`SupervisorLaneExchange::feed`]). Kept as a diagnostic: `build-id`
/// and the log/pane lines that name a peer's build still read this. A
/// bare crate version would not serve even that purpose — two different
/// commits can share one `Cargo.toml` version between releases — so
/// `build.rs` stamps `SOT_LOG_BUILD_SHA` from the FULL `git rev-parse
/// HEAD`, `-dirty`-suffixed over an uncommitted tree — the same
/// git-sha-capture pattern `rust/protocol/build.rs` uses for
/// `app_version` (copied rather than shared: `sot-log` must not depend on
/// `sot-protocol`, see `build.rs`'s own doc comment). `build.rs` itself
/// FAILS the build rather than emitting an empty/ambiguous value when no
/// git repository is found (Codex review round 2, finding M9: silently
/// falling back to the bare package version here made two different
/// commits sharing one pre-release version indistinguishable) — this
/// constant can therefore simply trust the env var is always a real,
/// nonempty identity, with no runtime fallback branch of its own. A
/// stronger, executable-hash-based identity is explicitly out of scope
/// ("Executable attestation — excluded by the threat model, not
/// deferred").
pub const SUPERVISOR_LANE_BUILD_ID: &str = env!("SOT_LOG_BUILD_SHA");

/// The supervisor lane's own `IdentityExchange` (ADR 0041 Lifecycle "The
/// challenge", steps 4-5): `hello {proto, build}` request, `hello_ok`
/// reply — this lane's identity-yielding exchange AND its protocol gate
/// (ADR 0045 decision 7: the gate is `proto` alone, `build` is
/// informational) in one round trip, per
/// [`wire::SupervisorRequest::Hello`]'s own doc ("doubles as the
/// same-connection challenge's own steps 4-5").
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
    /// never actually fail. Carried on the wire as information only
    /// (ADR 0045 decision 7) — never compared.
    build: String,
    /// The wire protocol integer this exchange gates on (ADR 0045
    /// decision 7: "the gate is the protocol integer"). Normally
    /// [`wire::SUPERVISOR_PROTO_V1`]; only [`Self::with_proto_for_test`]
    /// ever sets it to anything else.
    proto: u32,
    splitter: wire::FrameSplitter,
    done: bool,
    /// `true` iff the terminal `Foreign` this exchange reached was
    /// SPECIFICALLY a `Refused { reason: VersionSkew }` reply (ADR 0030
    /// §8 decision 31c) — the one `Foreign` cause that means "another
    /// lane protocol", as opposed to a malformed reply, trailing bytes, a
    /// wrong pid/creation, or any other corruption `feed` also
    /// classifies `Foreign`. `connect_and_challenge` (`supervisor_client.rs`)
    /// reads this via [`Self::is_version_skew`] after the challenge
    /// concludes.
    version_skew: bool,
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
        Self { build, proto: wire::SUPERVISOR_PROTO_V1, splitter: wire::FrameSplitter::new(), done: false, version_skew: false }
    }

    /// Test-only: an exchange that requests (and only accepts) a
    /// SPECIFIC `proto` instead of this build's own
    /// [`wire::SUPERVISOR_PROTO_V1`] — the one way to exercise a proto
    /// mismatch without a second real wire value. `build` still goes
    /// through [`Self::new`]'s own truncation.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_proto_for_test(build: impl Into<String>, proto: u32) -> Self {
        Self { proto, ..Self::new(build) }
    }

    /// See [`Self::version_skew`]'s own doc. Only meaningful once `feed`
    /// has reached a terminal `Foreign` outcome.
    pub fn is_version_skew(&self) -> bool {
        self.version_skew
    }
}

impl IdentityExchange for SupervisorLaneExchange {
    fn encode_request(&self) -> Vec<u8> {
        wire::encode_supervisor_request(&SupervisorRequest::Hello {
            proto: self.proto,
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
                        build: _,
                        pid,
                        created,
                    }) => {
                        // The gate is the protocol integer alone (ADR
                        // 0045 decision 7): this lane's request already
                        // carries OUR proto for the server to check; a
                        // `hello_ok` that doesn't echo it back is not
                        // proof of anything either — it could be a stale
                        // peer's reply to a DIFFERENT client's `hello`
                        // (or corruption) surviving just long enough to
                        // parse. `build` rides along as information only
                        // and is never compared — a different echoed
                        // build is fine.
                        if *proto == self.proto {
                            ExchangeDecode::Identity { pid: *pid, created: *created }
                        } else {
                            ExchangeDecode::Foreign
                        }
                    }
                    // The ONE `Foreign` cause that specifically means
                    // "another lane protocol" — see `version_skew`'s own
                    // doc. Every other well-formed-but-wrong reply below
                    // (a premature `StatusOk`, `Operation`, or a second
                    // `HelloOk`/`Refused`) stays plain `Foreign`.
                    DecodedFrame::SupervisorReply(SupervisorReply::Refused {
                        reason: wire::SupervisorRefusedReason::VersionSkew,
                    }) => {
                        self.version_skew = true;
                        ExchangeDecode::Foreign
                    }
                    _ => ExchangeDecode::Foreign, // any other well-formed reply, or corruption
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
#[path = "exchange_tests.rs"]
mod tests;
