//! The ConPTY host-facing handshake (ADR 0041 "Terminal state" +
//! "Step 4 as specified"): what current conhost emits at startup on
//! `hOutput` when the pseudoconsole is created with `dwFlags = 0` (no
//! `PSEUDOCONSOLE_INHERIT_CURSOR`) is DA1 — `ESC [ c`, with `ESC [ 0 c` as
//! its documented legal synonym — which the capsule answers with a fixed,
//! conservative VT identity: `ESC [ ? 1 ; 0 c`. This is the ONLY host-facing
//! query this project answers: CPR (`ESC [ 6 n`, cursor position report) is
//! NAMED here, not implemented — ConPTY only asks for it under
//! `PSEUDOCONSOLE_INHERIT_CURSOR`, which `conpty.rs::Pseudoconsole` never
//! sets, so there is no documented reason it would ever arrive. If that
//! assumption changes, answering CPR needs this module to track the LIVE
//! parser's cursor position too, which it deliberately does not do.
//!
//! Pure bytes, no OS/IO dependency, deliberately NOT `#[cfg(windows)]`: the
//! only real caller is `capsule_win.rs`, but the state machine itself has
//! nothing platform-specific in it, and gating its tests to the windows
//! CI legs would only make them run less often for no reason — they run on
//! every platform instead.
//!
//! WHY THIS FILE EXISTS despite the step-4 spec gate's "no separate
//! reusable DSR module" ruling — the tension is real and resolved
//! deliberately, not ignored: that ruling killed a REUSABLE, platform-
//! neutral module premised on the WRONG model (scanning producer output
//! for child queries). This is the corrected model's machine — host-facing
//! handshake only — and it lives in its own file for exactly one reason:
//! `capsule_win.rs` is `#![cfg(windows)]` at file level, so anything inside
//! it can only ever be tested on the two windows CI legs, while this
//! machine's whole risk surface (carry across arbitrary splits) is pure
//! bytes that a Linux dev fleet can regression-test on every native run.
//! Test reach earns the file; reusability is still not claimed — one
//! consumer, and the name says what it is, not "DSR".
//!
//! Byte-ordered, one byte at a time, with state carried across calls to
//! `feed`: the walkthrough this project follows is explicit that a query
//! is not guaranteed to arrive whole in one read, and the frontend's
//! existing "queries don't straddle chunks" shortcut is named in the ADR
//! as exactly the assumption this must not repeat.

/// DA1's fixed reply: `ESC [ ? 1 ; 0 c` — a conservative VT101-class
/// identity, not negotiated per session (pinned by the step-4 spec gate).
pub const DA1_REPLY: &[u8] = b"\x1b[?1;0c";

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

/// Byte-ordered carry-state detector + composer for the DA1 handshake.
/// `feed` is the whole API: hand it every output chunk, in order, and
/// write whatever it returns back into the pty's input immediately.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostHandshake {
    state: State,
}

impl HostHandshake {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk of host-ward (producer/ConPTY-emitted) bytes, in
    /// order. Returns the bytes to write into the pty's input — empty if
    /// nothing was recognized in this chunk. Recognizes both `ESC[c` and
    /// `ESC[0c` at ANY byte-boundary split, including one byte at a time,
    /// and correctly handles more than one query landing in a single chunk.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut reply = Vec::new();
        for &b in chunk {
            self.state = match (self.state, b) {
                // An ESC always (re)starts tracking, even mid-sequence: a
                // real query can be preceded by a false start (e.g. another
                // ESC never completed into `[`), and the LATEST ESC is the
                // one that can still turn into a real query.
                (_, 0x1b) => State::Esc,
                (State::Esc, b'[') => State::EscBracket,
                (State::EscBracket, b'c') => {
                    reply.extend_from_slice(DA1_REPLY);
                    State::Idle
                }
                (State::EscBracket, b'0') => State::EscBracketZero,
                (State::EscBracketZero, b'c') => {
                    reply.extend_from_slice(DA1_REPLY);
                    State::Idle
                }
                // Anything else at any state fails the match at this
                // position (including a non-`c` after `EscBracketZero` —
                // `ESC[0m` and friends are ordinary SGR codes, not ours).
                _ => State::Idle,
            };
        }
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `bytes` one byte per `feed()` call — the maximally adversarial
    /// split — and return every reply byte produced, concatenated in order.
    fn feed_one_byte_at_a_time(bytes: &[u8]) -> Vec<u8> {
        let mut hs = HostHandshake::new();
        let mut out = Vec::new();
        for &b in bytes {
            out.extend(hs.feed(&[b]));
        }
        out
    }

    #[test]
    fn recognizes_da1_short_form_split_at_every_byte() {
        assert_eq!(feed_one_byte_at_a_time(b"\x1b[c"), DA1_REPLY);
    }

    #[test]
    fn recognizes_da1_synonym_form_split_at_every_byte() {
        assert_eq!(feed_one_byte_at_a_time(b"\x1b[0c"), DA1_REPLY);
    }

    #[test]
    fn recognizes_da1_interleaved_with_other_output() {
        let mut hs = HostHandshake::new();
        let mut out = Vec::new();
        out.extend(hs.feed(b"hello \x1b[31mred\x1b[0m world "));
        out.extend(hs.feed(b"\x1b["));
        out.extend(hs.feed(b"c"));
        out.extend(hs.feed(b" more text"));
        assert_eq!(out, DA1_REPLY);
    }

    #[test]
    fn recognizes_two_queries_in_one_chunk() {
        let mut hs = HostHandshake::new();
        let out = hs.feed(b"\x1b[c\x1b[0c");
        let mut expected = DA1_REPLY.to_vec();
        expected.extend(DA1_REPLY);
        assert_eq!(out, expected);
    }

    #[test]
    fn ordinary_sgr_sequences_never_match() {
        // ESC[0m (reset) and ESC[31m (red) are extremely common in real
        // output and must never be mistaken for the query — this is the
        // same "precision, not `any ESC[`" lesson the conpty contract test
        // already applies.
        let mut hs = HostHandshake::new();
        let out = hs.feed(b"\x1b[0m\x1b[31m\x1b[1;1H");
        assert!(out.is_empty(), "false positive on ordinary CSI: {out:?}");
    }

    #[test]
    fn never_appearing_produces_no_reply() {
        let mut hs = HostHandshake::new();
        let out = hs.feed(b"plain text output, nothing to see here\n");
        assert!(out.is_empty());
    }

    #[test]
    fn a_false_start_esc_does_not_suppress_a_real_query_right_after() {
        // ESC on its own, followed by something that is NOT `[`, must not
        // leave the machine stuck — the NEXT real ESC[c must still match.
        let mut hs = HostHandshake::new();
        let out = hs.feed(b"\x1bX\x1b[c");
        assert_eq!(out, DA1_REPLY);
    }

    #[test]
    fn double_esc_before_bracket_still_matches() {
        // ESC ESC [ c: the first ESC is a false start, the second is the
        // one that actually leads into the query — an ESC must always
        // restart tracking, never be swallowed as "already in Esc state".
        let mut hs = HostHandshake::new();
        let out = hs.feed(b"\x1b\x1b[c");
        assert_eq!(out, DA1_REPLY);
    }
}
