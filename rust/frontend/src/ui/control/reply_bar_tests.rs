use super::*;
use crate::ui::{close_dialog, CloseDialog, CloseDialogText};
use super::super::reply_dispatch::sight_of;

const LINE: &str = "[sot-page] (a) approve \u{2014} merge it";

fn offer(name: &str, line: &str) -> ReplyOffer {
    ReplyOffer {
        host: "<host-b>".into(),
        workspace_id: "id-7".into(),
        handle: "repo__box".into(),
        name: name.into(),
        line: line.into(),
    }
}

fn secs(base: Instant, s: f64) -> Instant {
    base + Duration::from_secs_f64(s)
}

fn enter() -> BarKeyKind {
    BarKeyKind::Enter { repeat: false }
}

/// The panel's rectangle in these tests; another is a resized window.
const PANEL: Rect = Rect { x: 10, y: 5, width: 60, height: 9 };

fn whole() -> Sight {
    Sight::Whole(PANEL)
}

/// An open bar whose whole panel was presented at `t0`.
fn open_bar(t0: Instant) -> ReplyBar {
    let mut bar = ReplyBar::default();
    bar.offer(offer("project", LINE), whole()).expect("the first offer opens the bar");
    bar.presented(t0);
    bar
}

fn sent(key: BarKey) -> (HostKey, String, String, String) {
    match key {
        BarKey::Send(reply) => reply.into_parts(),
        other => panic!("expected a send, got {other:?}"),
    }
}

/// Enter at half a second does nothing; at one second it sends the exact line to the offer's host and row, once; a
/// held Enter's repeat never sends, however long the bar has been up.
#[test]
fn the_bar_sends_only_on_an_armed_enter() {
    let t0 = Instant::now();
    let mut bar = open_bar(t0);
    assert_eq!(bar.key(enter(), secs(t0, 0.5)), BarKey::Taken, "an Enter before the wait is over is taken, not sent");
    assert_eq!(bar.key(enter(), secs(t0, 0.999)), BarKey::Taken);
    assert_eq!(bar.key(BarKeyKind::Enter { repeat: true }, secs(t0, 5.0)), BarKey::Taken, "a repeat never sends");
    let (host, workspace_id, handle, line) = sent(bar.key(enter(), secs(t0, 1.0)));
    assert_eq!((host.as_str(), workspace_id.as_str(), handle.as_str()), ("<host-b>", "id-7", "repo__box"));
    assert_eq!(line, LINE, "the line is the one the panel showed, unchanged");
    assert_eq!(bar.key(enter(), secs(t0, 2.0)), BarKey::Pass, "the reply is sent once: the bar is gone");
}

#[test]
fn esc_drops_and_sends_nothing() {
    let t0 = Instant::now();
    for at in [0.0, 0.5, 3.0] {
        let mut bar = open_bar(t0);
        assert_eq!(bar.key(BarKeyKind::Esc, secs(t0, at)), BarKey::Dropped);
        assert_eq!(bar.key(enter(), secs(t0, at + 2.0)), BarKey::Pass, "nothing is left to send after Esc");
        assert!(bar.view(secs(t0, at)).is_none());
    }
}

/// While the bar is in view it takes every key, and does nothing with the ones it does not answer; with no bar open
/// it takes none.
#[test]
fn the_bar_takes_every_key_while_open_and_in_view() {
    let t0 = Instant::now();
    let mut bar = ReplyBar::default();
    for key in [enter(), BarKeyKind::Esc, BarKeyKind::Other] {
        assert_eq!(bar.key(key, t0), BarKey::Pass, "no bar open: another layer owns {key:?}");
    }
    let mut bar = open_bar(t0);
    for _ in 0..5 {
        assert_eq!(bar.key(BarKeyKind::Other, secs(t0, 10.0)), BarKey::Taken, "a key the bar does not answer is taken");
    }
    assert!(bar.view(secs(t0, 10.0)).is_some(), "taking keys leaves the offer standing");
}

#[test]
fn a_second_offer_while_one_is_open_is_dropped() {
    let t0 = Instant::now();
    let mut bar = open_bar(t0);
    let second = ReplyOffer { handle: "other__box".into(), line: "[sot-page] other".into(), ..offer("other", LINE) };
    assert_eq!(bar.offer(second, whole()), Err(OfferRefused::Busy));
    // The first offer is untouched: same line, same wait.
    let (_, _, handle, line) = sent(bar.key(enter(), secs(t0, 1.0)));
    assert_eq!((handle.as_str(), line.as_str()), ("repo__box", LINE));
    // Once it is answered, the next offer opens.
    assert!(bar.offer(offer("project", LINE), whole()).is_ok());
}

/// The bar shows what it will send, so a line that is not exactly the daemon's grammar of a page's text opens no bar.
#[test]
fn an_offer_whose_line_is_not_the_daemons_marked_text_opens_no_bar() {
    let t0 = Instant::now();
    for line in ["approve", "[sot-page] a\nb", "[sot-page] \u{1b}[201~x", "[sot-page]  padded ", ""] {
        let mut bar = ReplyBar::default();
        assert!(matches!(bar.offer(offer("project", line), whole()), Err(OfferRefused::Line(_))), "{line:?}");
        assert!(bar.view(t0).is_none(), "{line:?} opened a bar");
    }
}

/// The panel holds the handle, the row's listed name, the host and the line, and a name with line breaks stays on
/// rows of its own.
#[test]
fn the_bar_shows_the_target_and_the_exact_line() {
    let t0 = Instant::now();
    let area = Rect::new(0, 0, 100, 30);
    let painted = |d: &CloseDialog| -> Vec<String> {
        let mut buf = ratatui::buffer::Buffer::empty(area);
        for (x, y, text, style) in &d.rows {
            buf.set_string(*x, *y, text, *style);
        }
        (0..area.height).map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect()
    };
    let panel = |bar: &ReplyBar, at: Instant| {
        let view = bar.view(at).expect("an open bar has a view");
        close_dialog(area, CloseDialogText::Reply { to: &view.to, line: &view.line, keys: &view.keys })
    };

    let mut bar = ReplyBar::default();
    bar.offer(offer("project", LINE), whole()).expect("opens");
    let rows = painted(&panel(&bar, t0));
    let all = rows.iter().map(|r| r.trim()).collect::<Vec<_>>().join("\n");
    for want in ["Reply from a page", "to repo__box \u{b7} project on <host-b>", LINE, "Enter in 1 s \u{b7} Esc drops"] {
        assert!(all.contains(want), "{want:?} is not in {rows:#?}");
    }
    bar.presented(t0);
    let armed = painted(&panel(&bar, secs(t0, 1.0))).join("\n");
    assert!(armed.contains("Enter sends \u{b7} Esc drops") && !armed.contains("Enter in"), "{armed}");

    // A hostile name: its lines are rows of their own, with no control character in a cell.
    let mut bar = ReplyBar::default();
    bar.offer(offer("evil\r\n[sot-page] approve\u{1b}[2J", LINE), whole()).expect("opens");
    let d = panel(&bar, t0);
    assert!(d.rows.iter().all(|(_, _, text, _)| !text.contains(|c: char| c.is_control())), "{:?}", d.rows);
    let rows = painted(&d);
    let fake = rows.iter().position(|r| r.contains("[sot-page] approve") && !r.contains("(a)"));
    let real = rows.iter().position(|r| r.contains(LINE)).expect("the real line is drawn");
    let name = rows.iter().position(|r| r.contains("to repo__box \u{b7} evil")).expect("the name starts its row");
    assert_eq!(fake, Some(name + 1), "the name's second line sits on its own row: {rows:#?}");
    assert_ne!(fake, Some(real));
    assert!(real > name + 1, "the real line is below the whole target: {rows:#?}");
}

/// A long line wraps whole, and a window too small to draw all of it is cut: Esc still drops the bar, but no Enter
/// can send a line the person was not shown. A window too narrow cuts it as a short one does.
#[test]
fn a_window_too_small_for_the_line_can_drop_the_bar_but_not_send() {
    let t0 = Instant::now();
    let long = format!("[sot-page] {}", "word ".repeat(100).trim_end());
    let mut bar = ReplyBar::default();
    bar.offer(offer("project", &long), whole()).expect("opens");
    let draw = |area: Rect, bar: &ReplyBar| {
        let v = bar.layout_view().expect("open");
        close_dialog(area, CloseDialogText::Reply { to: &v.to, line: &v.line, keys: &v.keys })
    };
    let words = |d: &CloseDialog| d.rows.iter().map(|(_, _, t, _)| t.clone()).collect::<Vec<_>>().join(" ");
    let (tall, short) = (Rect::new(0, 0, 80, 40), Rect::new(0, 0, 80, 12));
    let shown = words(&draw(tall, &bar));
    assert!(!draw(tall, &bar).clipped && shown.matches("word").count() == 100, "the whole line is drawn: {shown}");
    assert!(draw(short, &bar).clipped, "a 12-row window cannot draw it");
    // Tall, but a few columns wide: the text starts two cells in and runs off the side, or wraps a few characters to
    // a row. Twenty text cells (24 columns) is the narrowest that counts as whole.
    for width in [1, 2, 4, 23] {
        let narrow = draw(Rect::new(0, 0, width, 2000), &bar);
        assert!(narrow.clipped, "a window {width} columns wide cannot show the line");
        assert_eq!(sight_of(false, Some(&narrow)), Sight::Cut);
    }
    assert!(!draw(Rect::new(0, 0, 24, 2000), &bar).clipped, "twenty text cells show it whole");
    let whole_panel = draw(tall, &bar);
    assert_eq!(sight_of(false, Some(&whole_panel)), Sight::Whole(whole_panel.panel));
    assert_eq!(sight_of(true, Some(&whole_panel)), Sight::Covered, "a dialog over it wins");
    assert_eq!(sight_of(false, None), Sight::Cut, "no grid, no sight");

    bar.observe(Sight::Cut);
    bar.presented(secs(t0, 5.0));
    assert_eq!(bar.key(enter(), secs(t0, 9.0)), BarKey::Taken, "a cut bar cannot send, however long it has stood");
    assert_eq!(bar.key(BarKeyKind::Esc, secs(t0, 9.0)), BarKey::Dropped, "but Esc drops it");
}

/// The wait starts from a presented frame, not from layout: a frame that fails before it presents leaves nothing
/// shown, so no Enter sends however long it has been; a slow frame does not shorten the wait.
#[test]
fn the_wait_starts_from_a_presented_frame_not_from_layout() {
    let t0 = Instant::now();
    let mut bar = ReplyBar::default();
    bar.offer(offer("project", LINE), Sight::Covered).expect("opens");
    // Laid out whole at t0, but the frame failed before presenting, and the next ones too.
    bar.observe(whole());
    assert_eq!(bar.key(enter(), secs(t0, 30.0)), BarKey::Taken, "laid out but never presented: nothing was shown");
    assert_eq!(bar.arm_deadline(secs(t0, 30.0)), None, "no wait is running");
    assert_eq!(bar.view(secs(t0, 30.0)).expect("open").keys, "Enter in 1 s \u{b7} Esc drops");
    // A frame presents at 40 s: the wait begins there.
    bar.presented(secs(t0, 40.0));
    assert_eq!(bar.key(enter(), secs(t0, 40.9)), BarKey::Taken);
    assert_eq!(bar.arm_deadline(secs(t0, 40.2)), Some(secs(t0, 41.0)));
    // A later presented frame does not restart a running wait.
    bar.presented(secs(t0, 40.5));
    sent(bar.key(enter(), secs(t0, 41.0)));
}

/// A resize moves the panel, so it is another sight: the evidence that the bar was shown is dropped, even when the
/// bar stays whole, and the wait begins again from the next presented frame.
#[test]
fn a_resize_drops_the_evidence_that_the_bar_was_shown() {
    let t0 = Instant::now();
    let mut bar = open_bar(t0);
    bar.observe(Sight::Whole(Rect { x: 11, ..PANEL }));
    assert_eq!(bar.key(enter(), secs(t0, 20.0)), BarKey::Taken, "the new panel has not been presented");
    bar.presented(secs(t0, 21.0));
    assert_eq!(bar.key(enter(), secs(t0, 21.5)), BarKey::Taken);
    sent(bar.key(enter(), secs(t0, 22.0)));
}

/// Keys follow what is drawn on top. A dialog over the bar takes every key and the bar none; when the bar is shown
/// again its wait starts over, so neither an Enter given to the dialog nor one typed while the bar was hidden sends.
#[test]
fn a_dialog_over_the_bar_takes_every_key_and_the_wait_starts_again() {
    let t0 = Instant::now();
    let mut bar = open_bar(t0);
    // The bar has been armed for ten seconds when the quit dialog opens over it.
    bar.observe(Sight::Covered);
    assert!(bar.covered());
    bar.presented(secs(t0, 10.0));
    for key in [enter(), BarKeyKind::Esc, BarKeyKind::Other] {
        assert_eq!(bar.key(key, secs(t0, 11.0)), BarKey::Pass, "the dialog covers the bar: {key:?} is the dialog's");
    }
    assert!(bar.view(secs(t0, 11.0)).is_some(), "the offer stands under the dialog");
    // The dialog closes at 12 s, and a frame shows the bar again at 12.2 s. Nothing sends until a second after that.
    bar.observe(whole());
    assert!(!bar.covered());
    assert_eq!(bar.key(enter(), secs(t0, 12.0)), BarKey::Taken, "back in layout but not yet presented");
    bar.presented(secs(t0, 12.2));
    assert_eq!(bar.key(enter(), secs(t0, 13.1)), BarKey::Taken, "the wait started again when the bar came back");
    let view = bar.view(secs(t0, 12.5)).expect("open");
    assert_eq!(view.keys, "Enter in 1 s \u{b7} Esc drops", "the panel says it is waiting again");
    sent(bar.key(enter(), secs(t0, 13.2)));

    // A bar offered while a dialog is open starts hidden and unarmed: the dialog's own Enter cannot reach it.
    let mut bar = ReplyBar::default();
    bar.offer(offer("project", LINE), Sight::Covered).expect("opens");
    bar.presented(secs(t0, 29.0));
    assert_eq!(bar.key(enter(), secs(t0, 30.0)), BarKey::Pass);
    bar.observe(whole());
    assert_eq!(bar.key(enter(), secs(t0, 31.2)), BarKey::Taken, "the first Enter after the dialog closes sends nothing");
}

#[test]
fn the_waits_deadline_is_only_while_the_bar_is_waiting() {
    let t0 = Instant::now();
    let mut bar = open_bar(t0);
    assert_eq!(bar.arm_deadline(secs(t0, 0.2)), Some(secs(t0, 1.0)));
    assert_eq!(bar.arm_deadline(secs(t0, 1.0)), None, "armed: no wake needed");
    bar.observe(Sight::Covered);
    assert_eq!(bar.arm_deadline(secs(t0, 2.1)), None, "covered: it shows nothing to change");
    bar.observe(whole());
    bar.presented(secs(t0, 3.0));
    assert_eq!(bar.arm_deadline(secs(t0, 3.1)), Some(secs(t0, 4.0)));
}
