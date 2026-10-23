//! Bounded lane I/O for the supervisor lane: `LaneError`, `write_bounded` and `FrameReader`.

use crate::lane::client::{transport_error_to_io, Client, Endpoint};
use crate::lane::wire::{self, DecodedFrame};
use std::collections::VecDeque;
use std::time::Instant;


// -----------------------------------------------------------------------
// Small bounded I/O helpers over any `Endpoint::Client`, shared by every
// lane this module speaks (mirrors the pattern each platform's own
// `challenge()` itself uses: `sot_platform::identity::deadline::run_with_deadline` racing
// the blocking call against a `cancel()`-issuing watchdog).
// -----------------------------------------------------------------------

/// `pub(crate)`: shared with [`run_end_run_and_wait`]'s own callers (ADR
/// 0042 L1a, Codex review finding 4 — one lane-I/O error vocabulary, not
/// two).
#[derive(Debug)]
#[allow(dead_code, reason = "the variants the attach worker produced are gone; 6c-1b deletes this module with the supervisor lane")]
pub(crate) enum LaneError {
    Io(std::io::Error),
    Timeout,
    Eof,
    Wire(wire::WireError),
    Protocol(&'static str),
    /// ADR 0045 decision 4: the daemon behind a `DaemonLaneEndpoint`
    /// dial refused `lane.connect` — `code`/`detail` kept SEPARATE
    /// (not joined into one string) so every caller can preserve the
    /// daemon's own diagnostic through to whatever terminal shape it
    /// produces, rather than a caller needing to re-parse a formatted
    /// string to recover the code.
    Refused { code: String, detail: String },
    /// ADR 0045 decision 4: the dial or handshake to the daemon's own
    /// lane bridge failed — retried after backoff, never charged to the
    /// absence window ([`ReconnectState::clear_unresponsive`]).
    Unreachable(String),
    /// ADR 0045 decision 4: the daemon answered `undetermined` — its own
    /// identity check on the lane it dialed could not complete. Retried
    /// exactly like `Unreachable`.
    Undetermined(String),
    /// ADR 0045 decision 4: the host's link is down and the dial started
    /// no ssh. Treated as a failed dial: no absence charge, backoff.
    LinkDown,
}

impl std::fmt::Display for LaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaneError::Io(e) => write!(f, "io: {e}"),
            LaneError::Timeout => write!(f, "timed out"),
            LaneError::Eof => write!(f, "connection closed"),
            LaneError::Wire(e) => write!(f, "wire: {e}"),
            LaneError::Protocol(s) => write!(f, "protocol: {s}"),
            LaneError::Refused { code, detail } => write!(f, "refused ({code}): {detail}"),
            LaneError::Unreachable(s) => write!(f, "unreachable: {s}"),
            LaneError::Undetermined(s) => write!(f, "undetermined: {s}"),
            LaneError::LinkDown => write!(f, "host link down"),
        }
    }
}

pub(crate) fn write_bounded<E: Endpoint>(conn: &E::Client, bytes: &[u8], deadline: Instant) -> Result<(), LaneError> {
    match sot_platform::identity::deadline::run_with_deadline(deadline, || conn.cancel(), || conn.write_all(bytes)) {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) => Err(LaneError::Io(transport_error_to_io(e))),
        None => Err(LaneError::Timeout),
    }
}

/// A connection plus its own `FrameSplitter` and a small pending queue,
/// so a bounded read never silently drops a SECOND frame that happened
/// to decode from the same underlying `read()` — unlike
/// `exchange::VoyageMgmtExchange`/`SupervisorLaneExchange` (whose
/// one-shot identity exchange treats a bundled second frame as
/// corruption, correctly, since THEIR protocol is exactly one round
/// trip), the mgmt/supervisor lane and the attach lane both keep being
/// used afterward, so a bundled extra frame here is ordinary traffic
/// that must be preserved for the caller's NEXT read. `pub(crate)`:
/// shared with [`run_end_run_and_wait`]'s own callers (ADR 0042 L1a).
pub(crate) struct FrameReader {
    splitter: wire::FrameSplitter,
    pending: VecDeque<DecodedFrame>,
}

impl FrameReader {
    pub(crate) fn new() -> Self {
        Self { splitter: wire::FrameSplitter::new(), pending: VecDeque::new() }
    }

    pub(crate) fn next_frame<E: Endpoint>(&mut self, conn: &E::Client, deadline: Instant) -> Result<DecodedFrame, LaneError> {
        if let Some(f) = self.pending.pop_front() {
            return Ok(f);
        }
        loop {
            let mut buf = [0u8; 8192];
            let n = match sot_platform::identity::deadline::run_with_deadline(deadline, || conn.cancel(), || conn.read(&mut buf)) {
                Some(Ok(n)) => n,
                Some(Err(e)) => return Err(LaneError::Io(transport_error_to_io(e))),
                None => return Err(LaneError::Timeout),
            };
            if n == 0 {
                return Err(LaneError::Eof);
            }
            let (frames, err) = self.splitter.feed(&buf[..n]);
            self.pending.extend(frames);
            if let Some(e) = err {
                return Err(LaneError::Wire(e));
            }
            if let Some(f) = self.pending.pop_front() {
                return Ok(f);
            }
        }
    }
}
