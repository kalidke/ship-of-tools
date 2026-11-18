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

/// Words a writer says again and again (a watcher's failure at each write of the previewed file) keep their first hold:
/// the connection line comes back when it ends, however long the writes go on.
#[test]
fn a_word_written_again_and_again_keeps_its_first_hold() {
    let t = Instant::now();
    let mut s = StatusLine::new("connected · h:row · rev 1");
    let mut connection_shown = 0;
    for k in 0..900u64 {
        let now = t + Duration::from_secs(k);
        if k % 4 == 0 {
            s.set_at("preview failed · run.h5: kernel_unavailable", now);
        }
        s.show_state_at("connected · h:row · rev 2", now);
        if s.expire_hold(now) {
            s.show_state_at("connected · h:row · rev 2", now);
        }
        connection_shown += usize::from(s.text().starts_with("connected"));
    }
    assert!(connection_shown > 0, "15 minutes of a file rewritten every 4 s never showed the connection line");
}

/// A person repeating one action still sees its words: a second press after the first one's hold ended holds them again.
#[test]
fn the_same_words_after_their_hold_ended_hold_the_line_again() {
    let t0 = Instant::now();
    let mut s = StatusLine::new("");
    s.set_at("pinned · files:a.txt", t0);
    s.show_state_at("connected · h:row · rev 1", t0 + NOTIFY_STICKY);
    assert_eq!(s.text(), "connected · h:row · rev 1");
    s.set_at("pinned · files:a.txt", t0 + NOTIFY_STICKY + Duration::from_secs(1));
    s.show_state_at("connected · h:row · rev 1", t0 + NOTIFY_STICKY + Duration::from_secs(9));
    assert_eq!(s.text(), "pinned · files:a.txt", "the second press's words did not hold the line");
}

/// The selected computer's own reconnect ends the hold of a disconnect line that carries a word, so the connection line
/// is written at once.
#[test]
fn a_reconnect_ends_the_hold_of_a_disconnect_line_that_carries_a_word() {
    let t0 = Instant::now();
    let mut s = StatusLine::new("connected · h:row · rev 1");
    s.set_at("pinned · files:a.txt", t0);
    let note = s.carried(t0 + Duration::from_secs(1)).expect("the word is carried");
    s.set_carrying("disconnected · eof · pinned · files:a.txt", note, t0 + Duration::from_secs(1));
    s.end_carry();
    s.show_state_at("connected · h:row · rev 2", t0 + Duration::from_secs(2));
    assert_eq!(s.text(), "connected · h:row · rev 2", "the disconnect line held the line after the reconnect");
}
