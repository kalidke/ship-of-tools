use super::*;
use std::time::{Duration, Instant};

/// A reply's outcome held on the status line survives the disconnect that follows it in the same frame.
#[test]
fn the_disconnect_line_carries_the_text_held_on_it() {
    assert_eq!(disconnect_status("eof", None), "disconnected \u{b7} eof");
    assert_eq!(
        disconnect_status("eof", Some("reply to repo__box: delivery unknown \u{b7} the connection to the daemon ended before it answered")),
        "disconnected \u{b7} eof \u{b7} reply to repo__box: delivery unknown \u{b7} the connection to the daemon ended before it answered"
    );
}

/// Two disconnect lines drain before one paint (a redial failing again): the reply's outcome is on the line after
/// both, once, and the first disconnect's reason is not.
#[test]
fn a_reply_outcome_survives_any_number_of_disconnect_lines() {
    let t0 = Instant::now();
    let mut status = StatusLine::new("connected");
    status.set_at("reply to repo__box: delivery unknown \u{b7} the connection ended", t0);
    for reason in ["eof", "dial failed", "dial failed again"] {
        show_disconnect(&mut status, reason, t0);
    }
    assert_eq!(
        status.text(),
        "disconnected \u{b7} dial failed again \u{b7} reply to repo__box: delivery unknown \u{b7} the connection ended"
    );
    // With nothing held, a disconnect carries nothing, and a disconnect line is never carried.
    let mut status = StatusLine::new("connected");
    show_disconnect(&mut status, "eof", t0);
    assert_eq!(status.text(), "disconnected \u{b7} eof");
    show_disconnect(&mut status, "eof again", t0);
    assert_eq!(status.text(), "disconnected \u{b7} eof again", "a disconnect line is never carried");
}

/// A disconnect with nothing held is a state line: it holds nothing, so the reconnect's connection line replaces it at once.
#[test]
fn a_disconnect_with_nothing_held_gives_way_to_the_connection_line() {
    let t0 = Instant::now();
    let mut s = StatusLine::new("connected · a:row · rev 1");
    show_disconnect(&mut s, "eof", t0);
    assert_eq!(s.text(), "disconnected \u{b7} eof");
    s.show_state_at("connected · a:row · rev 2", t0 + Duration::from_secs(1));
    assert_eq!(s.text(), "connected · a:row · rev 2", "the disconnect line held the line against the reconnect");
}
