use super::*;

/// Only `sent` says the line and its Enter were recorded; every other outcome says what became of the line.
#[test]
fn the_status_line_says_what_became_of_a_confirmed_reply() {
    assert_eq!(page_reply_status("repo__box", "sent", "the marked line and Enter were recorded"), "reply sent to repo__box");
    for (outcome, want) in [
        ("not_free", "reply to repo__box not sent \u{b7} a dialog is open"),
        ("refused", "reply to repo__box not sent \u{b7} a dialog is open"),
        ("error", "reply to repo__box not sent \u{b7} a dialog is open"),
        ("typed_not_sent", "reply to repo__box typed but not sent \u{b7} a dialog is open"),
        ("unknown", "reply to repo__box: delivery unknown \u{b7} a dialog is open"),
    ] {
        assert_eq!(page_reply_status("repo__box", outcome, "a dialog is open"), want, "{outcome}");
    }
}
