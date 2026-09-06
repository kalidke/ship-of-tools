//! ADR 0041's same-connection challenge (Lifecycle "The challenge"):
//! authenticates the server behind a live connection before any reply is
//! trusted, decoded for meaning, or acted on. This is the platform-neutral
//! core: the outcome vocabulary ([`ChallengeOutcome`]), the connection
//! trait every transport implements ([`ChallengeableConnection`]), and the
//! wire half of the challenge (`exchange_identity` — steps 4-5, the
//! deadline-bounded request/reply loop every lane shares). Steps 1-3 (the
//! OS-level peer-identity check) are necessarily platform-specific and
//! live in `challenge_win.rs` today; a `challenge_unix.rs` counterpart
//! lands in L1-unix's LU1c. Both platforms' own `challenge()`/
//! `authenticate_server()` call into THIS module's `exchange_identity`
//! for steps 4-5, so the wire behavior is provably identical everywhere,
//! not merely independently re-implemented per platform.
//!
//! U0 SCOPE (unchanged by the LU1a split): this module does NOT decide
//! what a `Proven`/`Foreign`/`Undetermined` result MEANS for readiness,
//! adoption, or respawn — the probe classifier's transition table (ADR
//! 0041 "The probe", Stage A/B) is a later unit's decision list, not this
//! one's.
//!
//! # Round-1 review: the exchange is a lane seam, not a lane
//!
//! Steps 1-3 (identify the peer process, authenticate its identity) are
//! the SAME procedure for every lane on a given platform, and stay
//! centralized in that platform's own `challenge()`. Only steps 4-5 (what
//! bytes to send, what bytes count as "the identity") vary per lane — the
//! voyage mgmt lane's own `status` request today, the supervisor lane's
//! own `status_ok {voyage, leg?, phase}` protocol later — so
//! [`crate::exchange::IdentityExchange`] is the one thing a lane provides,
//! and `exchange_identity` is the one thing every lane AND every
//! platform shares: a lane cannot skip or reorder the OS steps, because a
//! platform's own `challenge()` is the only caller of
//! `IdentityExchange::feed`, and it calls it only AFTER the OS-level
//! identity check has already matched. The deadline race itself
//! (`crate::deadline::run_with_deadline`) and the exchange trait/codec
//! ([`crate::exchange`]) are both portable — genuinely tested on every CI
//! platform, not merely compile-checked on Windows — and now
//! `exchange_identity` itself is too (L1-unix LU1a), leaving only the
//! actual OS-level authentication calls platform-specific.
//!
//! # U1a Codex round-1, Blocker 1 discharge: `Proven` is EARNED, not implied
//!
//! An earlier version of this module let `challenge()` take
//! `exchange: Option<...>`, with `None` running steps 1-3 alone and still
//! returning `ChallengeOutcome::Proven(ChallengedProcess)` — the SAME
//! success type and variant the full five-step exchange produces. Review
//! (Codex round 1, finding 1) called this a falsely-`Proven` result: a
//! same-identity pipe server that accepts a connection but never answers
//! anything was accepted immediately, with no liveness proof and no
//! pid/creation binding to the CONNECTION's own reply — exactly what steps
//! 4-5 exist to add. `Proven`/`ChallengedProcess` are RESERVED for the full
//! five-step exchange again; `crate::challenge_win::authenticate_server`
//! is the separately named, separately typed steps-1-3-only operation
//! `pipe_win::connect_voyage_pipe` now calls instead — see that function's
//! own doc for why the shared, lane-agnostic constructor can only ever
//! offer identity authentication, never the full proof, and for the
//! attach lane's own under-specification in the ADR.

use crate::deadline::run_with_deadline;
use crate::exchange::{ExchangeDecode, IdentityExchange};
use std::time::Instant;

/// One connection this crate can challenge: the blocking write/read/cancel
/// shape every pipe family already exposes — mirrors
/// `pipe_win::PipeClient`'s own public API as a trait, so this module
/// depends on neither pipe family by name (ADR 0041: "one procedure for
/// both pipe families"). `Sync`: the deadline watchdog below calls
/// `cancel()` from a second thread while the caller's thread blocks in
/// `read`/`write_all`. Raw handle access for the platform-specific
/// identity steps lives on the separate `PipeChallengeable`
/// (`crate::challenge_win::PipeChallengeable`) extension trait, not here —
/// see that trait's own doc.
pub trait ChallengeableConnection: Sync {
    fn write_all(&self, bytes: &[u8]) -> std::io::Result<()>;
    /// `Ok(0)` is ordered EOF — never a spurious zero-byte completion.
    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize>;
    /// Abort whatever is in flight, from another thread. One-shot: after
    /// this, the connection is not expected to serve a further request
    /// (matches `PipeClient::cancel`'s own latch-`Closing` contract) — a
    /// challenged connection is mgmt-lane-latched for its whole life
    /// anyway (ADR 0041 wire framing), so nothing legitimate reuses it
    /// past one `status` round trip.
    fn cancel(&self);
}

/// What the challenge concluded. Generic over the retained-process type
/// so `probe::ProbeOps` (an associated-type seam) can drive this same
/// three-way split with a cheap dummy type in tests, while the real
/// `crate::challenge_win::challenge` free function always instantiates
/// `ChallengeOutcome<crate::challenge_win::ChallengedProcess>`.
#[derive(Debug)]
pub enum ChallengeOutcome<P> {
    /// The peer's identity matched, and the reply's pid/creation matched
    /// what the OS independently observed on this connection (Windows:
    /// `GetNamedPipeServerProcessId` + `GetProcessTimes`). Carries the
    /// retained process handle — the ADR's death signal and pre-terminate
    /// re-verification both need a LIVE handle, not a remembered pid a
    /// later re-open could resolve to a recycled process.
    Proven(P),
    /// A well-formed WRONG answer: an identity mismatch, a wrong
    /// pid/creation, or anything `IdentityExchange::feed` classified
    /// `Foreign`. An unproven server — never retried as if it might still
    /// be legitimate.
    Foreign,
    /// Any OS-call failure (Windows: `GetNamedPipeServerProcessId`,
    /// `OpenProcess`, `OpenProcessToken`, `GetTokenInformation`,
    /// `GetProcessTimes`), EOF, a timeout, or a watchdog that could not
    /// even be established — anywhere in the five steps. Never classified
    /// as proven or foreign (ADR 0041: "a failure ... is PENDING, never
    /// READY and never ADOPTED").
    Undetermined,
}

/// Steps 4-5 of the same-connection challenge (ADR 0041 Lifecycle "The
/// challenge"): the lane's own request/reply, bounded by the shared
/// three-state watchdog (finding 4). `encode_request()` runs INSIDE the
/// deadline-bounded body (round-2 finding 2): an already-expired deadline
/// must never call the lane at all, and a blocking future lane's own
/// `encode_request()` must not be able to defeat the bound by running
/// before the watchdog even exists. Extracted (L1-unix LU1a) so every
/// platform's own `challenge()` — steps 1-3 are platform-specific; this
/// half is not — shares the identical wire behavior instead of each
/// independently re-implementing it.
///
/// `cfg_attr`: no non-Windows caller exists yet — `challenge_win::challenge`
/// is the only one today, until LU1c's `challenge_unix.rs` lands (same
/// device LU0 used for `transport.rs`'s own hoisted-but-not-yet-called
/// items).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn exchange_identity(
    conn: &dyn ChallengeableConnection,
    exchange: &mut dyn IdentityExchange,
    reply_deadline: Instant,
) -> Option<Result<(u32, u64), StatusFailure>> {
    run_with_deadline(
        reply_deadline,
        || conn.cancel(),
        move || -> Result<(u32, u64), StatusFailure> {
            let request = exchange.encode_request();
            conn.write_all(&request).map_err(|_| StatusFailure::Undetermined)?;
            let mut buf = [0u8; 512];
            loop {
                let n = conn.read(&mut buf).map_err(|_| StatusFailure::Undetermined)?;
                if n == 0 {
                    return Err(StatusFailure::Undetermined); // ordered EOF mid-challenge
                }
                match exchange.feed(&buf[..n]) {
                    ExchangeDecode::Incomplete => continue,
                    ExchangeDecode::Identity { pid, created } => return Ok((pid, created)),
                    ExchangeDecode::Foreign => return Err(StatusFailure::Foreign),
                }
            }
        },
    )
}

/// The peer's identity once SID-authenticated (steps 1-3 ONLY) — pid plus
/// creation time read directly off the OS handle, NEVER off a reply (there
/// is none). Deliberately a plain data struct with NO retained handle: the
/// capabilities `ChallengedProcess` offers (`reverify`/`wait`/`terminate`)
/// all depend on the full five-step proof's LIVE handle, and this weaker
/// operation earns none of them — a caller that needs those must run the
/// full `crate::challenge_win::challenge` itself. Deliberately NOT named
/// `Proven` and NOT `ChallengedProcess` (U1a Codex round-1, Blocker 1): the
/// two operations must never be typed identically, so no consumer can
/// mistake SID-only authentication for the full reply-bound liveness
/// proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidAuthenticated {
    pub pid: u32,
    pub created: u64,
}

/// What `crate::challenge_win::authenticate_server` concluded — a
/// SEPARATE enum from [`ChallengeOutcome`] (not a generic instantiation of
/// it) for the same reason `SidAuthenticated` is a separate type: nothing
/// here is ever spelled `Proven`.
#[derive(Debug)]
pub enum SidAuthOutcome {
    /// The peer's token-user SID matches this account's.
    Authenticated(SidAuthenticated),
    /// A well-formed WRONG answer: the peer's SID differs. Never retried
    /// as if it might still be legitimate.
    Foreign,
    /// An OS-call failure anywhere in steps 1-3. Never classified as
    /// authenticated or foreign.
    Undetermined,
}

/// `exchange_identity`'s own failure vocabulary for its bounded body —
/// `pub(crate)` (L1-unix LU1a): every platform's own `challenge()` module
/// matches on it after calling `exchange_identity`. `cfg_attr`: same
/// no-non-Windows-caller-yet reasoning as `exchange_identity` above.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum StatusFailure {
    Foreign,
    Undetermined,
}
