use super::*;

/// Feed `bytes` one byte per `feed()` call — the maximally adversarial
/// split — and return the total match count across every call.
fn feed_one_byte_at_a_time(bytes: &[u8]) -> usize {
    let mut hs = HostHandshake::new();
    bytes.iter().map(|&b| hs.feed(&[b])).sum()
}

#[test]
fn recognizes_da1_short_form_split_at_every_byte() {
    assert_eq!(feed_one_byte_at_a_time(b"\x1b[c"), 1);
}

#[test]
fn recognizes_da1_synonym_form_split_at_every_byte() {
    assert_eq!(feed_one_byte_at_a_time(b"\x1b[0c"), 1);
}

#[test]
fn recognizes_da1_interleaved_with_other_output() {
    let mut hs = HostHandshake::new();
    let mut total = 0usize;
    total += hs.feed(b"hello \x1b[31mred\x1b[0m world ");
    total += hs.feed(b"\x1b[");
    total += hs.feed(b"c");
    total += hs.feed(b" more text");
    assert_eq!(total, 1);
}

#[test]
fn recognizes_two_queries_in_one_chunk() {
    let mut hs = HostHandshake::new();
    assert_eq!(hs.feed(b"\x1b[c\x1b[0c"), 2);
}

#[test]
fn ordinary_sgr_sequences_never_match() {
    // ESC[0m (reset) and ESC[31m (red) are extremely common in real
    // output and must never be mistaken for the query — this is the
    // same "precision, not `any ESC[`" lesson the conpty contract test
    // already applies.
    let mut hs = HostHandshake::new();
    let n = hs.feed(b"\x1b[0m\x1b[31m\x1b[1;1H");
    assert_eq!(n, 0, "false positive on ordinary CSI");
}

#[test]
fn never_appearing_produces_no_reply() {
    let mut hs = HostHandshake::new();
    assert_eq!(hs.feed(b"plain text output, nothing to see here\n"), 0);
}

#[test]
fn a_false_start_esc_does_not_suppress_a_real_query_right_after() {
    // ESC on its own, followed by something that is NOT `[`, must not
    // leave the machine stuck — the NEXT real ESC[c must still match.
    let mut hs = HostHandshake::new();
    assert_eq!(hs.feed(b"\x1bX\x1b[c"), 1);
}

#[test]
fn double_esc_before_bracket_still_matches() {
    // ESC ESC [ c: the first ESC is a false start, the second is the
    // one that actually leads into the query — an ESC must always
    // restart tracking, never be swallowed as "already in Esc state".
    let mut hs = HostHandshake::new();
    assert_eq!(hs.feed(b"\x1b\x1b[c"), 1);
}
