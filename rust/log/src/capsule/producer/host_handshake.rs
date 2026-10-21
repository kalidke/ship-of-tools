//! The ConPTY host-facing handshake (ADR 0041 "Terminal state" +
//! "Step 4 as specified"): what current conhost emits at startup on
//! `hOutput` when the pseudoconsole is created with `dwFlags = 0` (no
//! `PSEUDOCONSOLE_INHERIT_CURSOR`) is DA1 — `ESC [ c`, with `ESC [ 0 c` as
//! its documented legal synonym — which the capsule answers with a fixed,
//! conservative VT identity: `ESC [ ? 1 ; 0 c`. This is the ONLY host-facing
//! query this project answers: CPR (`ESC [ 6 n`, cursor position report) is
//! NAMED here, not implemented — ConPTY only asks for it under
//! `PSEUDOCONSOLE_INHERIT_CURSOR`, which `capsule/producer/conpty/mod.rs::Pseudoconsole` never
//! sets, so there is no documented reason it would ever arrive. If that
//! assumption changes, answering CPR needs this module to track the LIVE
//! parser's cursor position too, which it deliberately does not do.
//!
//! Pure bytes, no OS/IO dependency, deliberately NOT `#[cfg(windows)]`: the
//! state machine itself has
//! nothing platform-specific in it, and gating its tests to the windows
//! CI legs would only make them run less often for no reason — they run on
//! every platform instead.
//!
//! CRATE-PRIVATE: the ADR's
//! "one private machine" ruling for the host-facing handshake means this
//! module's items must not be part of the crate's PUBLIC API even though
//! the file itself is public in the sense of being its own translation
//! unit.
//!
//! WHY THIS FILE EXISTS despite the step-4 spec gate's "no separate
//! reusable DSR module" ruling — the tension is real and resolved
//! deliberately, not ignored: that ruling killed a REUSABLE, platform-
//! neutral module premised on the WRONG model (scanning producer output
//! for child queries). This is the corrected model's machine — host-facing
//! handshake only — and it lives in its own file for exactly one reason:
//! this machine's whole risk surface (carry across arbitrary splits) is pure
//! bytes that a Linux dev fleet can regression-test on every native run.
//! Test reach earns the file; reusability is still not claimed — one
//! consumer, and the name says what it is, not "DSR".
//!
//! Byte-ordered, one byte at a time, with state carried across calls to
//! `feed`: the walkthrough this project follows is explicit that a query
//! is not guaranteed to arrive whole in one read, and the frontend's
//! existing "queries don't straddle chunks" shortcut is named in the ADR
//! as exactly the assumption this must not repeat.
//!
//! `feed` returns a MATCH COUNT, not reply bytes:
//! the reply is always the same fixed `DA1_REPLY` constant regardless of
//! which of the two forms matched, so the caller needs only "how many
//! matched in this chunk" to decide what to do — and per ADR 0041's
//! model (conhost asks ONCE, at startup), the caller's policy is to answer
//! and record only the FIRST match ever observed for a run, suppressing
//! (but counting) any later ones. This module only ever reports what it saw.

/// DA1's fixed reply: `ESC [ ? 1 ; 0 c` — a conservative VT101-class
/// identity, not negotiated per session (pinned by the step-4 spec gate).
pub(crate) const DA1_REPLY: &[u8] = b"\x1b[?1;0c";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    /// Nothing matched yet (or the last attempt failed to match).
    #[default]
    Idle,
    /// Saw ESC.
    Esc,
    /// Saw ESC [.
    EscBracket,
    /// Saw ESC [ 0 — one more `c` completes the 4-byte synonym.
    EscBracketZero,
}

/// Byte-ordered carry-state detector for the DA1 handshake. `feed` is the
/// whole API: hand it every output chunk, in order, and it reports how
/// many times a query matched within that chunk (0, 1, or more).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HostHandshake {
    state: State,
}

impl HostHandshake {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk of host-ward (producer/ConPTY-emitted) bytes, in
    /// order. Returns how many times `ESC[c` or `ESC[0c` matched in this
    /// chunk — 0 if nothing was recognized. Recognizes both forms at ANY
    /// byte-boundary split, including one byte at a time, and correctly
    /// counts more than one query landing in a single chunk.
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> usize {
        let mut matches = 0usize;
        for &b in chunk {
            self.state = match (self.state, b) {
                // An ESC always (re)starts tracking, even mid-sequence: a
                // real query can be preceded by a false start (e.g. another
                // ESC never completed into `[`), and the LATEST ESC is the
                // one that can still turn into a real query.
                (_, 0x1b) => State::Esc,
                (State::Esc, b'[') => State::EscBracket,
                (State::EscBracket, b'c') => {
                    matches += 1;
                    State::Idle
                }
                (State::EscBracket, b'0') => State::EscBracketZero,
                (State::EscBracketZero, b'c') => {
                    matches += 1;
                    State::Idle
                }
                // Anything else at any state fails the match at this
                // position (including a non-`c` after `EscBracketZero` —
                // `ESC[0m` and friends are ordinary SGR codes, not ours).
                _ => State::Idle,
            };
        }
        matches
    }
}

#[cfg(test)]
#[path = "host_handshake_tests.rs"]
mod tests;
