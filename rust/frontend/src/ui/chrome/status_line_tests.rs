use super::*;
use std::time::Duration;

const MS: Duration = Duration::from_millis(1);

/// A word holds the line for `NOTIFY_STICKY` from its own write: a state line written before then leaves it, one written
/// at the hold's end replaces it, and a state line holds nothing.
#[test]
fn words_hold_the_line_against_a_state_line_until_their_hold_ends() {
    let t0 = Instant::now();
    let mut s = StatusLine::new("connected · a:row · rev 1");
    s.set_at("not posted: the preview shows no file", t0);
    s.show_state_at("connected · a:row · rev 2", t0 + NOTIFY_STICKY - MS);
    assert_eq!(s.text(), "not posted: the preview shows no file", "a state line replaced a held word");
    s.show_state_at("connected · a:row · rev 2", t0 + NOTIFY_STICKY);
    assert_eq!(s.text(), "connected · a:row · rev 2", "the state line did not come back when the hold ended");
    s.show_state_at("connected · a:row · rev 3", t0 + NOTIFY_STICKY + MS);
    assert_eq!(s.text(), "connected · a:row · rev 3", "a state line held the line");
}

/// A newer word replaces a held one at once, and its hold runs from its own write, not the older word's.
#[test]
fn newer_words_replace_held_words_at_once_and_hold_from_their_own_time() {
    let t0 = Instant::now();
    let mut s = StatusLine::new("");
    s.set_at("upload · a.png 1/2", t0);
    s.set_at("upload · a.png 2/2", t0 + Duration::from_secs(8));
    assert_eq!(s.text(), "upload · a.png 2/2", "a newer word waited behind a held one");
    s.show_state_at("connected · a:row · rev 2", t0 + Duration::from_secs(12));
    assert_eq!(s.text(), "upload · a.png 2/2", "the newer word's hold ran from the older word's write");
    s.show_state_at("connected · a:row · rev 2", t0 + Duration::from_secs(8) + NOTIFY_STICKY);
    assert_eq!(s.text(), "connected · a:row · rev 2");
}

/// The idle tick ends a hold once, at its time, and not before.
#[test]
fn a_hold_ends_once_at_its_time() {
    let t0 = Instant::now();
    let mut s = StatusLine::new("");
    s.set_at("copied 3 lines", t0);
    assert!(!s.expire_hold(t0 + NOTIFY_STICKY - MS), "the hold ended early");
    assert!(s.expire_hold(t0 + NOTIFY_STICKY), "the hold did not end at its time");
    assert!(!s.expire_hold(t0 + NOTIFY_STICKY + MS), "the hold ended twice");
}

/// A clear empties the line and ends its hold, so the next state line is written.
#[test]
fn a_clear_empties_the_line_and_ends_its_hold() {
    let t0 = Instant::now();
    let mut s = StatusLine::new("");
    s.set_at("relaunching…", t0);
    s.clear();
    assert_eq!(s.text(), "");
    s.show_state_at("connected · a:row · rev 2", t0 + Duration::from_secs(1));
    assert_eq!(s.text(), "connected · a:row · rev 2", "a cleared line still held");
}
