//! `Parser::is_ground` boundary truth table (ADR 0041 step 5).
//!
//! The capsule can only cut an attach stream where the escape-sequence state
//! machine is at ground and no partial UTF-8 codepoint is buffered — anywhere
//! else, the fresh parser on the other side would be missing bytes it needs
//! to make sense of what follows.
//!
//! Each boundary case proves two properties, and both are needed:
//!
//! 1. **Chunking invariance.** Splitting a byte string anywhere and feeding
//!    the pieces to one continuously-running parser renders the same screen
//!    as feeding it in one shot. This holds at *every* split point, legal or
//!    not — a live parser never "loses" anything by having `process` called
//!    twice instead of once. That is exactly why, on its own, it cannot tell
//!    a legal cut from an illegal one: an early version of this file checked
//!    only this, with both halves fed to the same parser, so even a cut
//!    mid-CSI passed.
//! 2. **The checkpoint-cut contract.** A checkpoint taken at the legal cut,
//!    restored into a brand new parser with no memory of anything before
//!    it, must render whatever comes next identically to the original
//!    parser continuing in place. This is the property a real capsule
//!    attach actually depends on, and it only holds at a true ground
//!    boundary — an illegal cut would restore into a parser missing the
//!    partial escape sequence or codepoint the original was mid-way
//!    through.

fn contents_of(bytes: &[u8]) -> String {
    let mut parser = vt100_ctt::Parser::new(10, 40, 0);
    parser.process(bytes);
    parser.screen().contents()
}

/// The checkpoint-cut contract (property 2 above). `parser` must already be
/// at ground. Checkpoints it, restores that checkpoint into a brand new
/// parser, feeds `trailing` to both, and requires their final checkpoints to
/// agree. Leaves `trailing` fed into `parser` on return, so callers can go on
/// to make further assertions about it.
fn assert_restore_matches_continuation(
    parser: &mut vt100_ctt::Parser,
    trailing: &[u8],
) {
    assert!(parser.is_ground(), "must cut at ground to restore");
    let bytes = parser.screen().checkpoint().expect("checkpoint");

    let mut restored = vt100_ctt::Parser::default();
    restored.restore_screen(&bytes).expect("restore");

    parser.process(trailing);
    restored.process(trailing);

    assert_eq!(
        parser.screen().checkpoint().expect("checkpoint"),
        restored.screen().checkpoint().expect("checkpoint"),
        "restoring at a ground cut and continuing diverged from continuing \
         in place after feeding {trailing:?}",
    );
}

/// Drives one ground/not-ground boundary case, proving both properties from
/// the same case data.
///
/// `prefix` is fed first and must land somewhere NOT ground (an illegal cut
/// point). `to_ground` is then fed and must complete to ground (the legal
/// cut point). `trailing` is fed last, purely to drive the two properties —
/// it is never itself asserted on for ground-ness.
fn assert_ground_boundary(prefix: &[u8], to_ground: &[u8], trailing: &[u8]) {
    // Property 1: chunking never changes rendering — checked with one
    // parser run straight through prefix, to_ground, and trailing.
    let mut parser = vt100_ctt::Parser::new(10, 40, 0);
    parser.process(prefix);
    assert!(!parser.is_ground(), "expected NOT ground after {prefix:?}");
    parser.process(to_ground);
    assert!(
        parser.is_ground(),
        "expected ground after {prefix:?} then {to_ground:?}"
    );
    parser.process(trailing);

    let whole: Vec<u8> =
        prefix.iter().chain(to_ground).chain(trailing).copied().collect();
    assert_eq!(
        parser.screen().contents(),
        contents_of(&whole),
        "chunking {whole:?} changed the parsed screen",
    );

    // Property 2: the checkpoint-cut contract — only meaningful, and only
    // holds, at the legal cut (after prefix + to_ground).
    let up_to_cut: Vec<u8> =
        prefix.iter().chain(to_ground).copied().collect();
    let mut at_cut = vt100_ctt::Parser::new(10, 40, 0);
    at_cut.process(&up_to_cut);
    assert_restore_matches_continuation(&mut at_cut, trailing);
}

#[test]
fn mid_escape() {
    assert_ground_boundary(b"\x1b", b"7", b"ok");
}

#[test]
fn mid_csi_after_bracket() {
    assert_ground_boundary(b"\x1b[", b"31m", b"text");
}

#[test]
fn mid_csi_after_parameter_byte() {
    // Same sequence as `mid_csi_after_bracket`, cut one byte later: partway
    // through the "31" parameter rather than right after "[".
    assert_ground_boundary(b"\x1b[3", b"1m", b"text");
}

#[test]
fn mid_escape_intermediate() {
    // ESC ( : Escape's intermediate-collect range, landing in
    // EscapeIntermediate rather than dispatching directly. 'B' (0x42) is in
    // EscapeIntermediate's `0x30..=0x7e => (Ground, EscDispatch)` range.
    assert_ground_boundary(b"\x1b(", b"B", b"ok");
}

#[test]
fn mid_csi_intermediate() {
    // ESC [ 1 (space): CsiEntry -> CsiParam on the digit, then CsiParam's
    // `0x20..=0x2f => (CsiIntermediate, Collect)` on the space. 'q' (0x71)
    // is in CsiIntermediate's `0x40..=0x7e => (Ground, CsiDispatch)` range.
    assert_ground_boundary(b"\x1b[1 ", b"q", b"ok");
}

#[test]
fn mid_csi_ignore() {
    // CORRECTED PREFIX: a colon here (`\x1b[1:`) does NOT reach CsiIgnore —
    // CsiParam's `0x3a..=0x3b => (Anywhere, Param)` treats colon as an
    // ordinary subparameter separator (e.g. `38:2:255:0:255`), staying in
    // CsiParam. CsiIgnore is reached instead by a private-marker byte
    // (0x3c..=0x3f) arriving after a parameter has already started:
    // CsiParam's `0x3c..=0x3f => (CsiIgnore, None)`. '<' (0x3c) after the
    // "1" parameter does that. CsiIgnore's own final-byte rule is
    // `0x40..=0x7e => (Ground, None)` — no CSI dispatch fires at all, the
    // whole malformed sequence is swallowed.
    assert_ground_boundary(b"\x1b[1<", b"m", b"ok");
}

#[test]
fn mid_osc_st_terminated() {
    assert_ground_boundary(b"\x1b]0;ti", b"tle\x1b\\", b"ok");
}

#[test]
fn mid_osc_bel_terminated() {
    assert_ground_boundary(b"\x1b]0;ti", b"tle\x07", b"ok");
}

#[test]
fn mid_dcs_entry() {
    // ESC P q: DCS introducer with no parameters, dispatched straight into
    // passthrough. Default `Perform::hook`/`put`/`unhook` are no-ops, so the
    // sequence carries no visible content of its own.
    assert_ground_boundary(b"\x1bPq", b"passthrough data\x1b\\", b"ok");
}

#[test]
fn mid_dcs_passthrough() {
    // Cut deeper into the same sequence, after passthrough bytes have
    // already been consumed rather than right at the DCS introducer.
    assert_ground_boundary(b"\x1bPqpassthrough", b" data\x1b\\", b"ok");
}

#[test]
fn mid_dcs_param() {
    // ESC P 1: DcsEntry's `0x30..=0x39 => (DcsParam, Param)` on the digit.
    // Never reaches DcsPassthrough, so hook/unhook never fire — ST (ESC \)
    // still reaches ground because ESC is in vte's `Anywhere` row and takes
    // priority from any state, landing in `Escape`, and `\` (0x5c) is
    // `Escape`'s own `(Ground, EscDispatch)` byte.
    assert_ground_boundary(b"\x1bP1", b"\x1b\\", b"ok");
}

#[test]
fn mid_dcs_intermediate() {
    // ESC P 1 (space): DcsEntry -> DcsParam on the digit, then DcsParam's
    // `0x20..=0x2f => (DcsIntermediate, Collect)` on the space.
    assert_ground_boundary(b"\x1bP1 ", b"\x1b\\", b"ok");
}

#[test]
fn mid_dcs_ignore() {
    // CORRECTED PREFIX: same colon mistake as `mid_csi_ignore` — DcsEntry's
    // `0x3a..=0x3b => (DcsParam, Param)` treats a leading colon as an
    // ordinary parameter, not a route to DcsIgnore. DcsIgnore is reached by
    // a private-marker byte (0x3c..=0x3f) after a parameter has started:
    // DcsParam's `0x3c..=0x3f => (DcsIgnore, None)`. '<' after the "1"
    // parameter does that. Payload bytes fed while in DcsIgnore are ignored
    // (`0x00..=0x7f => (Anywhere, Ignore)`) — false through all of them,
    // ground only after ST.
    assert_ground_boundary(b"\x1bP1<ignored payload", b"\x1b\\", b"ok");
}

/// SosPmApcString (entered via ESC X / ESC ^ / ESC _) ignores essentially
/// every byte until ST, the same shape as DcsIgnore. That is exactly why it
/// matters here: bytes the live parser silently drops in this state —
/// ordinary printable text among them — would become printable output if an
/// erroneous fresh-parser cut landed inside the payload instead of at the
/// terminating ST, since a fresh parser handed only the tail would have no
/// idea it was ever inside an ignored string and would print it instead.
mod sos_pm_apc_string {
    #[test]
    fn via_sos() {
        super::assert_ground_boundary(
            b"\x1bXignored payload",
            b"\x1b\\",
            b"ok",
        );
    }

    #[test]
    fn via_pm() {
        super::assert_ground_boundary(
            b"\x1b^ignored payload",
            b"\x1b\\",
            b"ok",
        );
    }

    #[test]
    fn via_apc() {
        super::assert_ground_boundary(
            b"\x1b_ignored payload",
            b"\x1b\\",
            b"ok",
        );
    }
}

#[test]
fn utf8_three_byte_continuations() {
    // "日" = E6 97 A5. Ground only reappears once all three bytes have
    // landed; the two continuation bytes are `State::Utf8`, not `Ground`.
    let mut parser = vt100_ctt::Parser::new(10, 40, 0);
    parser.process(&[0xe6]);
    assert!(!parser.is_ground(), "not ground after the lead byte alone");
    parser.process(&[0x97]);
    assert!(!parser.is_ground(), "not ground after two of three bytes");
    parser.process(&[0xa5]);
    assert!(parser.is_ground(), "ground once the codepoint completed");

    assert_restore_matches_continuation(&mut parser, b"!");
    assert_eq!(parser.screen().contents(), contents_of("日!".as_bytes()));
}

#[test]
fn utf8_four_byte_continuations() {
    // "\u{1F600}" (grinning face) = F0 9F 98 80.
    let bytes = "\u{1F600}".as_bytes();
    assert_eq!(bytes, [0xf0, 0x9f, 0x98, 0x80]);

    let mut parser = vt100_ctt::Parser::new(10, 40, 0);
    parser.process(&bytes[..1]);
    assert!(!parser.is_ground(), "not ground after the lead byte alone");
    parser.process(&bytes[1..2]);
    assert!(!parser.is_ground(), "not ground after two of four bytes");
    parser.process(&bytes[2..3]);
    assert!(!parser.is_ground(), "not ground after three of four bytes");
    parser.process(&bytes[3..4]);
    assert!(parser.is_ground(), "ground once the codepoint completed");

    assert_restore_matches_continuation(&mut parser, b"!");
    assert_eq!(
        parser.screen().contents(),
        contents_of("\u{1F600}!".as_bytes())
    );
}

#[test]
fn can_cancels_mid_csi_to_ground() {
    // CAN (0x18) is in vte's `Anywhere` row, so it applies from any state,
    // including partway through a CSI sequence with parameters already
    // collected.
    assert_ground_boundary(b"\x1b[3;1", b"\x18", b"ok");
}

#[test]
fn sub_cancels_mid_csi_to_ground() {
    // SUB (0x1a) is CAN's `Anywhere`-row twin.
    assert_ground_boundary(b"\x1b[3;1", b"\x1a", b"ok");
}

/// DEL (0x7f) as the last byte of a ground-state chunk still leaves the
/// parser at ground — but not because DEL is ignored. In vte's table, DEL
/// from `Ground` lands in the very same `(Anywhere, Print)` cell as any
/// ordinary 7-bit character; the byte is printed, and `Ground` simply never
/// left. That is also why callback-inference — watching which `Perform`
/// method fired and guessing ground-ness from it — was rejected as an
/// implementation strategy: DEL is `Action::Ignore` in other states
/// (`CsiEntry`, `Escape`, ...) and `Action::Print` here, so "something
/// harmless happened" does not distinguish "still at ground" from "mid
/// sequence, byte discarded." Only reading the state machine's actual state
/// answers that, which is the entire reason this fork vendors it.
#[test]
fn del_in_ground_is_still_ground() {
    let mut parser = vt100_ctt::Parser::new(10, 40, 0);
    parser.process(b"hello\x7f");
    assert!(parser.is_ground(), "DEL from ground must not leave ground");

    assert_restore_matches_continuation(&mut parser, b" more");
    assert_eq!(parser.screen().contents(), contents_of(b"hello\x7f more"));
}

#[test]
fn plain_text_ending_in_ascii_is_ground() {
    let mut parser = vt100_ctt::Parser::new(10, 40, 0);
    parser.process(b"hel");
    assert!(parser.is_ground(), "plain ASCII text never leaves ground");
    parser.process(b"lo");
    assert!(parser.is_ground(), "plain ASCII text never leaves ground");

    assert_restore_matches_continuation(&mut parser, b"!");
    assert_eq!(parser.screen().contents(), contents_of(b"hello!"));
}
