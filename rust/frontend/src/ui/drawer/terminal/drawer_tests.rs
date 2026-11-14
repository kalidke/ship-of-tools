//! The Terminal drawer's eased scroll over a real shell's ring: a row at a time, ended by typing.

use super::*;

const LENGTH: Duration = Duration::from_millis(150);

fn after(t: Instant, ms: u64) -> Instant { t + Duration::from_millis(ms) }
fn back(d: &TerminalDrawer) -> usize { d.screen().expect("shell").scrollback() }

/// A drawer whose shell has printed 80 lines (so 56 are in the ring of a 24-row screen) and exited.
fn printed() -> TerminalDrawer {
    let mut d = TerminalDrawer::new(None);
    let shell = ResolvedShell { program: "/bin/sh".to_string(), args: vec!["-c".to_string(), "seq 1 80".to_string()] };
    d.spawn(&shell, Box::new(|| {})).expect("spawn /bin/sh");
    let give_up = Instant::now() + Duration::from_secs(5);
    loop {
        d.pump();
        let shown = d.screen().expect("shell").contents();
        if shown.lines().any(|l| l.trim() == "80") { return d; }
        assert!(Instant::now() < give_up, "the shell's output never reached the screen");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_page_scrolls_the_ring_a_row_at_a_time_and_typing_returns_at_once() {
    let t = Instant::now();
    let mut d = printed();
    d.scroll(8, t, LENGTH);
    assert_eq!(back(&d), 0, "nothing moves at the event itself");
    // The first row shows at the first frame, and the page comes to rest on its eight rows.
    assert!(d.spend_scroll(after(t, 8)));
    assert!(back(&d) >= 1 && back(&d) < 8, "{}", back(&d));
    let mut ms = 16;
    while d.spend_scroll(after(t, ms)) { ms += 8; assert!(ms < 10_000); }
    assert_eq!(back(&d), 8);
    assert!(ms >= 152, "the length runs out at its end: {ms}");
    // The live tail ends a scroll toward it, so nothing is owed past the edge.
    d.scroll(-40, after(t, 1000), LENGTH);
    d.spend_scroll(after(t, 1008));
    d.spend_scroll(after(t, 1016));
    let mut ms = 1024;
    while d.spend_scroll(after(t, ms)) { ms += 8; assert!(ms < 10_000); }
    assert_eq!(back(&d), 0);
    // Typing returns to the live tail at once and ends what is owed.
    d.scroll(8, after(t, 2000), LENGTH);
    d.spend_scroll(after(t, 2008));
    assert!(back(&d) > 0);
    d.type_input(b"x");
    assert_eq!(back(&d), 0);
    assert!(!d.spend_scroll(after(t, 2016)));
    assert_eq!(back(&d), 0, "a stopped motion owes nothing");
    // A zero length is today's: all of it at the event.
    d.scroll(5, after(t, 3000), Duration::ZERO);
    assert_eq!(back(&d), 5);
    assert!(!d.spend_scroll(after(t, 3008)));
}
