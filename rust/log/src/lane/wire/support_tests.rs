//! Helpers the wire test files share: golden comparison and splitter feeds.

use super::*;

pub(super) fn assert_golden(wire: Vec<u8>, expected_hex_bytes: &[u8]) {
    assert_eq!(wire, expected_hex_bytes);
}

/// Feeds bytes expected to decode cleanly, panicking with a useful
/// message if a wire error surfaced instead of the frames.
pub(super) fn feed_ok(s: &mut FrameSplitter, bytes: &[u8]) -> Vec<DecodedFrame> {
    let (frames, err) = s.feed(bytes);
    assert_eq!(err, None, "unexpected wire error");
    frames
}

/// Feeds bytes expected to produce no frames and exactly one error.
pub(super) fn feed_err(s: &mut FrameSplitter, bytes: &[u8]) -> WireError {
    let (frames, err) = s.feed(bytes);
    assert!(frames.is_empty(), "expected no frames before the error, got {frames:?}");
    err.expect("expected a wire error")
}
