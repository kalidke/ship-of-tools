//! The Terminal drawer's eased scroll over a real shell's ring: a row at a time, ended by typing.

use super::*;

const LENGTH: Duration = Duration::from_millis(150);

fn after(t: Instant, ms: u64) -> Instant { t + Duration::from_millis(ms) }
fn back(d: &TerminalDrawer) -> usize { d.screen().expect("shell").scrollback() }

/// A drawer whose shell ran `script` and exited, once `shown` holds of its screen.
fn ran(script: &str, shown: impl Fn(&vt100::Screen) -> bool) -> TerminalDrawer {
    let mut d = TerminalDrawer::new(None);
    let shell = ResolvedShell { program: "/bin/sh".to_string(), args: vec!["-c".to_string(), script.to_string()] };
    d.spawn(&shell, Box::new(|| {})).expect("spawn /bin/sh");
    let give_up = Instant::now() + Duration::from_secs(5);
    loop {
        d.pump();
        if shown(d.screen().expect("shell")) { return d; }
        assert!(Instant::now() < give_up, "the shell's output never reached the screen");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 80 lines printed, so 56 of them are in the ring of a 24-row screen.
fn printed() -> TerminalDrawer { ran("seq 1 80", |s| s.contents().lines().any(|l| l.trim() == "80")) }

/// Frames every 8 ms from `ms` until the scroll rests; how many came after the one that last moved the ring.
fn idle_frames(d: &mut TerminalDrawer, t: Instant, mut ms: u64) -> u32 {
    let mut idle = 0;
    loop {
        let before = back(d);
        let moving = d.spend_scroll(after(t, ms));
        idle = if back(d) != before { 0 } else { idle + 1 };
        if !moving { return idle; }
        ms += 8;
        assert!(ms < 10_000, "a scroll that never rests");
    }
}

#[test]
fn a_page_scrolls_the_ring_a_row_at_a_time_and_typing_returns_at_once() {
    let t = Instant::now();
    let mut d = printed();
    d.scroll(8, t, LENGTH);
    assert_eq!(back(&d), 0, "nothing moves at the event itself");
    // The first row shows at the first frame, and the page comes to rest on its eight rows, with the last of them.
    assert!(d.spend_scroll(after(t, 8)));
    assert!(back(&d) >= 1 && back(&d) < 8, "{}", back(&d));
    assert_eq!(idle_frames(&mut d, t, 16), 0, "the roll ends with the frame that shows its last row");
    assert_eq!(back(&d), 8);
    // The live tail ends a scroll toward it at the frame that meets it, so nothing is owed past the edge.
    let t = after(t, 1000);
    d.scroll(-40, t, LENGTH);
    assert!(!d.spend_scroll(after(t, 8)));
    assert_eq!(back(&d), 0);
    // Typing returns to the live tail at once and ends what is owed, even inside the length.
    let t = after(t, 1000);
    d.scroll(8, t, LENGTH);
    assert!(d.spend_scroll(after(t, 8)));
    assert!(back(&d) > 0);
    d.type_input(b"x");
    assert_eq!(back(&d), 0);
    assert!(!d.spend_scroll(after(t, 16)));
    assert_eq!(back(&d), 0, "a stopped motion owes nothing");
    // A zero length is today's: all of it at the event.
    d.scroll(5, after(t, 1000), Duration::ZERO);
    assert_eq!(back(&d), 5);
    assert!(!d.spend_scroll(after(t, 1008)));
}

#[test]
fn a_reshape_stops_a_roll_and_a_program_on_the_alternate_screen_is_moved_at_once() {
    let t = Instant::now();
    let mut d = printed();
    d.fit(80, 24);
    d.scroll(8, t, LENGTH);
    assert!(d.spend_scroll(after(t, 8)));
    d.fit(80, 22);
    assert!(!d.roll.moving() && !d.spend_scroll(after(t, 16)), "a reshape ends what is owed");
    // A program that owns the screen is never eased: nothing is owed after the ask.
    let mut d = ran("seq 1 80; printf '\\033[?1049h'", |s| s.alternate_screen());
    d.scroll(8, t, LENGTH);
    assert!(!d.roll.moving());
}
