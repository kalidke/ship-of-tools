//! Bounded lane I/O: `LaneError`, `write_bounded`, `FrameReader` and the attach-refusal wording.

use crate::client::{transport_error_to_io, Client, Endpoint};
use crate::transport::TransportError;
use crate::wire::{self, DecodedFrame};
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::time::Instant;


// -----------------------------------------------------------------------
// Small bounded I/O helpers over any `Endpoint::Client`, shared by every
// lane this module speaks (mirrors the pattern each platform's own
// `challenge()` itself uses: `crate::deadline::run_with_deadline` racing
// the blocking call against a `cancel()`-issuing watchdog).
// -----------------------------------------------------------------------

/// `pub(crate)`: shared with [`run_end_run_and_wait`]'s own callers (ADR
/// 0042 L1a, Codex review finding 4 — one lane-I/O error vocabulary, not
/// two).
#[derive(Debug)]
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
    /// The capsule refused the attach, carrying the reason IT named
    /// rather than collapsing into `Protocol("attach_refused")`. The
    /// reason is not decoration: `SubscriberCap` held by orphaned watcher
    /// connections never clears on its own, while `GroundTimeout` clears
    /// within seconds — a caller that cannot tell the two apart retries
    /// an unretryable state forever with nothing naming the cause.
    AttachRefused(wire::AttachRefusedReason),
}

/// The pane wording for an `attach_refused` reason — the ONE place either
/// reason becomes words, shared by [`LaneError`]'s own `Display` and the
/// episode's status emit. Deliberately does not quote the capsule's
/// subscriber cap as a number: that bound lives in `attach_proto` (and is
/// scheduled to change), while the user's move does not depend on its
/// value, and deliberately promises nothing about what a restart costs:
/// restarting the daemon frees the held slots without ending a session
/// whose supervisor got its own transient systemd scope (ADR 0043
/// decision 32 — `capsule_workspace`'s Linux `spawn_detached`), but a
/// supervisor launched after `user_scope_available()` was DENIED runs
/// degraded in the daemon's own kill domain and dies with it. One pane
/// status line cannot carry that fork, so the reassurance and its
/// exception both live in the troubleshooting page instead.
pub(crate) fn attach_refused_text(reason: wire::AttachRefusedReason) -> &'static str {
    match reason {
        wire::AttachRefusedReason::GroundTimeout => "capsule busy grounding a checkpoint \u{2014} retrying\u{2026}",
        wire::AttachRefusedReason::SubscriberCap => {
            "capsule watcher slots all taken \u{2014} restart the backend daemon to free them"
        }
    }
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
            LaneError::AttachRefused(r) => write!(f, "attach refused: {}", attach_refused_text(*r)),
        }
    }
}

/// The three bridge-only `TransportError` arms (ADR 0045 decision 4),
/// classified BEFORE any conversion through [`transport_error_to_io`] —
/// unwrapping `Unreachable`/a bridge-sourced `Undetermined` that way
/// would make transport uncertainty look exactly like the absence
/// `TransportError::is_endpoint_absent()` reports, folding a hiccup and
/// a genuinely dead row into one signal (the invariant this whole
/// decision exists to keep). Every other `TransportError` — the
/// platform endpoints' own `Io`/`RuntimeDir`/etc., including a
/// direct-sourced `Undetermined`/the unit `Foreign` a CHALLENGED
/// connect can still produce elsewhere in this crate, never from an
/// `Endpoint::connect_*_unchallenged` call — keeps going through
/// [`transport_error_to_io`] exactly as before this decision landed.
/// `Refused{code: "unauthenticated", ..}` reaching here is ALWAYS a
/// bridge-speaking daemon's own bad-token refusal — `sot_protocol::topology::
/// lane_client::classify_reply` already renames an old daemon's
/// coincidentally-`unauthenticated`-coded control-loop gate to
/// `no_bridge` at the source, so this function never has to re-guess it
/// from message text.
pub(super) fn classify_transport(e: TransportError) -> LaneError {
    match e {
        TransportError::Refused { code, detail } => LaneError::Refused { code, detail },
        TransportError::Unreachable(io) => LaneError::Unreachable(io.to_string()),
        TransportError::Undetermined { detail, .. } => LaneError::Undetermined(detail),
        TransportError::LinkDown => LaneError::LinkDown,
        other => LaneError::Io(transport_error_to_io(other)),
    }
}

pub(super) fn is_access_denied(e: &std::io::Error) -> bool {
    // ERROR_ACCESS_DENIED == 5 is a Windows GetLastError code -- on Linux
    // os error 5 is EIO, unrelated, so the raw-code check must not apply
    // there (L1-unix LU3b: this function is now reachable from a Linux
    // build too). `ErrorKind::PermissionDenied` (std's own EACCES/EPERM
    // mapping) alone is both necessary and sufficient on Linux.
    (cfg!(windows) && e.raw_os_error() == Some(5)) || e.kind() == ErrorKind::PermissionDenied
}

pub(crate) fn write_bounded<E: Endpoint>(conn: &E::Client, bytes: &[u8], deadline: Instant) -> Result<(), LaneError> {
    match crate::deadline::run_with_deadline(deadline, || conn.cancel(), || conn.write_all(bytes)) {
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
            let n = match crate::deadline::run_with_deadline(deadline, || conn.cancel(), || conn.read(&mut buf)) {
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
