//! `Parser::is_ground` boundary truth table (ADR 0041 step 5).
//!
//! The capsule can only cut an attach stream where the escape-sequence state
//! machine is at ground and no partial UTF-8 codepoint is buffered — anywhere
//! else, the fresh parser on the other side would be missing bytes it needs
//! to make sense of what follows. Every case here feeds a byte string in two
//! pieces, checks `is_ground()` at the cut, then proves the cut was invisible
//! to parsing: the split run and an unsplit run of the same bytes must render
//! the same screen. That last check is what makes this a test of `is_ground`
//! and not just of the underlying vendored state machine — a wrong answer
//! from `is_ground` wouldn't touch screen contents at all, only which byte
//! offsets a real capsule would be allowed to cut at.

fn contents_of(bytes: &[u8]) -> String {
    let mut parser = vt100_ctt::Parser::new(10, 40, 0);
    parser.process(bytes);
    parser.screen().contents()
}

/// Feeds `prefix` then `remainder` to a fresh parser, asserting `is_ground()`
/// at each of the three points given, then checks that the split produced
/// the same screen as feeding `prefix` and `remainder` concatenated to a
/// separate parser in one shot.
fn assert_ground_boundary(
    prefix: &[u8],
    after_prefix: bool,
    remainder: &[u8],
    after_remainder: bool,
) {
    let mut parser = vt100_ctt::Parser::new(10, 40, 0);
    parser.process(prefix);
    assert_eq!(
        parser.is_ground(),
        after_prefix,
        "is_ground() after {prefix:?} should have been {after_prefix}"
    );
    parser.process(remainder);
    assert_eq!(
        parser.is_ground(),
        after_remainder,
        "is_ground() after {prefix:?} then {remainder:?} should have been {after_remainder}"
    );

    let whole: Vec<u8> = prefix.iter().chain(remainder).copied().collect();
    assert_eq!(
        parser.screen().contents(),
        contents_of(&whole),
        "splitting {whole:?} at byte {} changed the parsed screen",
        prefix.len()
    );
}

#[test]
fn mid_escape() {
    assert_ground_boundary(b"\x1b", false, b"7ok", true);
}

#[test]
fn mid_csi_after_bracket() {
    assert_ground_boundary(b"\x1b[", false, b"31mtext", true);
}

#[test]
fn mid_csi_after_parameter_byte() {
    // Same sequence as `mid_csi_after_bracket`, cut one byte later: partway
    // through the "31" parameter rather than right after "[".
    assert_ground_boundary(b"\x1b[3", false, b"1mtext", true);
}

#[test]
fn mid_osc_st_terminated() {
    assert_ground_boundary(b"\x1b]0;ti", false, b"tle\x1b\\ok", true);
}

#[test]
fn mid_osc_bel_terminated() {
    assert_ground_boundary(b"\x1b]0;ti", false, b"tle\x07ok", true);
}

#[test]
fn mid_dcs_entry() {
    // ESC P q: DCS introducer with no parameters, dispatched straight into
    // passthrough. Default `Perform::hook`/`put`/`unhook` are no-ops, so the
    // sequence carries no visible content of its own.
    assert_ground_boundary(b"\x1bPq", false, b"passthrough data\x1b\\ok", true);
}

#[test]
fn mid_dcs_passthrough() {
    // Cut deeper into the same sequence, after passthrough bytes have
    // already been consumed rather than right at the DCS introducer.
    assert_ground_boundary(b"\x1bPqpassthrough", false, b" data\x1b\\ok", true);
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
    parser.process(b"!");
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
    parser.process(b"!");
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
    assert_ground_boundary(b"\x1b[3;1", false, b"\x18ok", true);
}

#[test]
fn sub_cancels_mid_csi_to_ground() {
    // SUB (0x1a) is CAN's `Anywhere`-row twin.
    assert_ground_boundary(b"\x1b[3;1", false, b"\x1aok", true);
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
    assert_eq!(
        parser.screen().contents(),
        contents_of(b"hello\x7f"),
        "splitting around a trailing DEL changed the parsed screen"
    );
}

#[test]
fn plain_text_ending_in_ascii_is_ground() {
    assert_ground_boundary(b"hel", true, b"lo", true);
}
