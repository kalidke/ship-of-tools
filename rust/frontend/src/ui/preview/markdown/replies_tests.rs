use super::*;
use crate::ui::chrome::StatusLine;
use crate::ui::preview::notice::PreviewNotice;
use std::time::{Duration, Instant};

/// A markdown file with two broken equations that another program saves every 3 s: each save renders the page again and
/// both equations fail again, in turn. Their words go to the preview's box, and the status line keeps the connection line.
#[test]
fn two_broken_equations_saved_again_and_again_never_hold_the_status_line() {
    let t = Instant::now();
    let mut status = StatusLine::new("connected · h:row · rev 1");
    let mut notice = PreviewNotice::default();
    let mut connection_s = 0;
    for k in 0..900u64 {
        let now = t + Duration::from_secs(k);
        if k % 3 == 0 {
            say_math_failure(&mut notice, &mut status, "Undefined control sequence \\R", now);
            say_math_failure(&mut notice, &mut status, "Missing close brace", now);
        }
        if status.expire_hold(now) {
            status.show_state_at("connected · h:row · rev 1", now);
        }
        connection_s += usize::from(status.text().starts_with("connected"));
    }
    assert_eq!(connection_s, 900, "the equations' words held the status line {} of 900 s", 900 - connection_s);
    assert_eq!(notice.text(), Some("math · Missing close brace"), "the box does not say the last failure");
}
