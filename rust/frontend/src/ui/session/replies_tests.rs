use super::*;

/// A reply's outcome held on the status line survives the disconnect that follows it in the same frame.
#[test]
fn the_disconnect_line_carries_the_text_held_on_it() {
    assert_eq!(disconnect_status("eof", None), "disconnected \u{b7} eof");
    assert_eq!(
        disconnect_status("eof", Some("reply to repo__box: delivery unknown \u{b7} the connection to the daemon ended before it answered")),
        "disconnected \u{b7} eof \u{b7} reply to repo__box: delivery unknown \u{b7} the connection to the daemon ended before it answered"
    );
}

#[test]
fn a_held_disconnect_line_is_not_carried_into_the_next_one() {
    assert_eq!(disconnect_status("eof", Some("disconnected \u{b7} eof")), "disconnected \u{b7} eof");
}
