use super::*;
use crate::ui::chrome::StatusLine;
use crate::ui::preview::notice::PreviewNotice;
use std::time::{Duration, Instant};

/// A preview's fetch failing again at each re-fetch the person did not ask for (a watcher's change of a file another program
/// writes), its words changing each time (two texts in turn, then a counter): the box says each, and the status line keeps
/// the connection line.
#[test]
fn a_refetch_failure_whose_words_change_never_holds_the_status_line() {
    let t = Instant::now();
    let mut status = StatusLine::new("connected · h:row · rev 1");
    let mut notice = PreviewNotice::default();
    let mut connection_s = 0;
    for k in 0..900u64 {
        let now = t + Duration::from_secs(k);
        let message = if k < 450 { ["kernel_unavailable", "too large"][(k % 2) as usize].to_string() } else { format!("try {k}") };
        say_preview_failure(&mut notice, &mut status, Some("files:out/run.h5"), &message, true, now);
        if status.expire_hold(now) {
            status.show_state_at("connected · h:row · rev 1", now);
        }
        connection_s += usize::from(status.text().starts_with("connected"));
    }
    assert_eq!(connection_s, 900, "the re-fetch's failures held the status line {} of 900 s", 900 - connection_s);
    assert_eq!(notice.text(), Some("preview failed · run.h5: try 899"));
}

/// A fetch the person asked for (selecting the file) that fails is said on the status line as well, as a word.
#[test]
fn a_failure_answering_the_persons_own_fetch_is_a_word_on_the_status_line() {
    let t = Instant::now();
    let mut status = StatusLine::new("connected · h:row · rev 1");
    let mut notice = PreviewNotice::default();
    say_preview_failure(&mut notice, &mut status, Some("files:out/run.h5"), "kernel_unavailable", false, t);
    assert_eq!(status.text(), "preview failed · run.h5: kernel_unavailable");
    assert_eq!(notice.text(), Some("preview failed · run.h5: kernel_unavailable"));
    status.show_state_at("connected · h:row · rev 1", t + Duration::from_secs(5));
    assert_eq!(status.text(), "preview failed · run.h5: kernel_unavailable", "the person's failure did not hold the line");
}
