use super::*;
use sot_protocol::ops::{PageWord, PageWords};

fn word(l: u16, t: u16, r: u16, b: u16, text: &str) -> PageWord {
    PageWord { left: l, top: t, right: r, bottom: b, text: text.into() }
}

/// "alpha beta" on a line near the page's top and "gamma" lower, as a page's words.
fn words(page: u32) -> PageWords {
    PageWords {
        page,
        cut: false,
        lines: vec![
            vec![word(6554, 6554, 19661, 13107, "alpha"), word(32768, 6554, 45875, 13107, "beta")],
            vec![word(6554, 32768, 19661, 39322, "gamma")],
        ],
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> ScreenRect {
    ScreenRect { x, y, w, h }
}

fn key(page: u32) -> Option<PageKey> {
    Some(("host".into(), Some("row".into()), "files:a.pdf".into(), page))
}

/// The page drawn 1000 by 2000 at (100, 50), the whole of it in view.
const CANVAS: ScreenRect = ScreenRect { x: 100.0, y: 50.0, w: 1000.0, h: 2000.0 };

fn shown(page: u32) -> PdfSelect {
    let mut s = PdfSelect::default();
    s.sync(key(page));
    s.set_frame(CANVAS, rect(0.0, 0.0, 5000.0, 5000.0));
    s
}

/// Screen positions of a page fraction.
fn at(x: f32, y: f32) -> (f32, f32) {
    (CANVAS.x + x * CANVAS.w, CANVAS.y + y * CANVAS.h)
}

fn host() -> HostKey { "host".into() }

/// The press on `p` that must ask for the words: the number it asks under.
fn asks(s: &mut PdfSelect, p: (f32, f32)) -> u64 {
    s.press(p).expect("on the page").expect("the words are asked for")
}

const SCANNED: &str = "There is no text to select on this page.";

/// A press on the page asks for its words once, under a number: not again while the request is out, nor once they are held;
/// a request that ended is asked again at the next press under a new number; a press off the page's rectangle, off the
/// visible image, under a box drawn over it, before a frame or with no page shown, is not the words'.
#[test]
fn a_press_on_a_pdf_page_asks_once_for_its_words() {
    let mut s = shown(2);
    let first = asks(&mut s, at(0.2, 0.1));
    s.released();
    assert_eq!(s.press(at(0.2, 0.1)), Some(None), "the request is out");
    s.released();
    assert_eq!(s.arrived(PageWordsReply::Ended, first, &host()), None);
    let second = asks(&mut s, at(0.2, 0.1));
    assert_ne!(first, second, "an ended request is asked again under a new number");
    assert_eq!(s.arrived(PageWordsReply::Words(words(2)), second, &host()), None);
    assert_eq!(s.text(), None, "words that come under a press with no motion select nothing");
    assert_eq!(s.press(at(0.2, 0.1)), Some(None), "the words are held");
    // Off the page's rectangle, off the visible image, under a box, before a frame, and with no page shown.
    assert_eq!(s.press((5.0, 5.0)), None);
    s.set_frame(CANVAS, rect(0.0, 0.0, 300.0, 300.0));
    assert_eq!(s.press(at(0.5, 0.5)), None, "on the page but under the pane's edge");
    s.set_frame(CANVAS, rect(0.0, 0.0, 5000.0, 5000.0));
    s.set_cover(Some(rect(at(0.1, 0.05).0, at(0.1, 0.05).1, 400.0, 400.0)));
    assert_eq!(s.press(at(0.2, 0.1)), None, "a box drawn over the page takes the press");
    s.set_cover(None);
    assert!(s.press(at(0.2, 0.1)).is_some());
    s.forget();
    assert_eq!(s.press(at(0.2, 0.1)), None, "after the page is installed and before its first frame");
    let mut none = PdfSelect::default();
    none.set_frame(CANVAS, rect(0.0, 0.0, 5000.0, 5000.0));
    assert_eq!(none.press(at(0.2, 0.1)), None);
}

/// An answer is taken only for the request asked: a number never asked, an old one after the page was asked again, and one
/// that came after the page was dropped and shown anew, select nothing even with a drag and words held.
#[test]
fn a_reply_is_taken_only_for_the_request_asked() {
    let mut s = shown(2);
    let old = asks(&mut s, at(0.15, 0.1));
    s.released();
    s.forget();
    s.set_frame(CANVAS, rect(0.0, 0.0, 5000.0, 5000.0));
    let new = asks(&mut s, at(0.15, 0.1));
    s.moved(at(0.55, 0.1));
    assert_eq!(s.arrived(PageWordsReply::Words(words(2)), old, &host()), None);
    assert_eq!((s.text(), s.selected()), (None, false), "the old request's words mark nothing");
    s.arrived(PageWordsReply::Words(words(2)), new, &host());
    assert_eq!(s.text().as_deref(), Some("alpha beta"), "the request asked is taken");
    s.arrived(PageWordsReply::Ended, new, &host());
    assert!(s.selected(), "an answer to a request no longer out changes nothing");
}

/// A drag selects the words between its ends, marks them and gives their text, as the words arrive after the press too,
/// and after the release too (a drag that moved before they came); a press and release with no motion, and a move to the
/// press's own position, select nothing.
#[test]
fn a_drag_over_the_words_selects_marks_and_gives_their_text() {
    let mut s = shown(2);
    let n = asks(&mut s, at(0.15, 0.1));
    assert!(s.moved(at(0.55, 0.1)), "motion before the words came is kept");
    assert_eq!((s.text(), s.bands().len()), (None, 0));
    s.arrived(PageWordsReply::Words(words(2)), n, &host());
    assert_eq!(s.text().as_deref(), Some("alpha beta"), "the drag's ends, from where the press and the pointer are now");
    assert_eq!(s.bands().len(), 1);
    s.moved(at(0.15, 0.55));
    s.released();
    assert_eq!(s.text().as_deref(), Some("alpha beta\ngamma"), "kept after the button is up");
    assert_eq!(s.bands().len(), 2);
    assert!(!s.moved(at(0.15, 0.1)), "motion after the release is no drag's");
    asks_none(&mut s, at(0.15, 0.1));
    assert_eq!(s.text(), None, "a press clears the last selection");
    assert!(!s.moved(at(0.15, 0.1)), "a move to the press's own position is not motion");
    // The frame changes under the held button (a zoom or a resize): a move to the press's own screen position is still none.
    let mut z = shown(2);
    let p = at(0.15, 0.1);
    asks(&mut z, p);
    z.set_frame(rect(300.0, 150.0, 2000.0, 4000.0), rect(0.0, 0.0, 5000.0, 5000.0));
    assert!(!z.moved(p), "a stationary pointer is no motion, whatever the page's rectangle became");
    z.released();
    assert_eq!(z.text(), None);
    s.released();
    assert_eq!((s.text(), s.bands().len()), (None, 0), "a press and release with no motion selects nothing");
    // The same drag run backward selects the same words.
    s.press(at(0.15, 0.55));
    s.moved(at(0.15, 0.1));
    s.released();
    assert_eq!(s.text().as_deref(), Some("alpha beta\ngamma"));
    // A drag that moved and was released before the words came selects them when they come; a click does not.
    let mut t = shown(2);
    let n = asks(&mut t, at(0.15, 0.1));
    t.moved(at(0.55, 0.1));
    t.released();
    assert!(!t.moved(at(0.15, 0.55)), "the drag kept for the words is not the pointer's any more");
    t.arrived(PageWordsReply::Words(words(2)), n, &host());
    assert_eq!(t.text().as_deref(), Some("alpha beta"));
    let mut u = shown(2);
    let n = asks(&mut u, at(0.15, 0.1));
    u.released();
    u.arrived(PageWordsReply::Words(words(2)), n, &host());
    assert_eq!(u.text(), None);
}

fn asks_none(s: &mut PdfSelect, p: (f32, f32)) { assert_eq!(s.press(p), Some(None)); }

/// A selection goes when another press is made elsewhere or Escape is pressed (`clear`), and a drag ends at the window's loss
/// of focus or a lost release (`released`): the pointer then selects nothing more.
#[test]
fn a_selection_is_cleared_elsewhere_and_a_lost_release_ends_the_drag() {
    let mut s = shown(2);
    let n = asks(&mut s, at(0.15, 0.1));
    s.arrived(PageWordsReply::Words(words(2)), n, &host());
    s.moved(at(0.55, 0.1));
    s.released();
    assert!(s.selected());
    s.clear();
    assert_eq!((s.text(), s.selected(), s.bands().len()), (None, false, 0));
    // The button is held, the window loses focus, the release never comes: the next motion selects nothing.
    asks_none(&mut s, at(0.15, 0.1));
    s.moved(at(0.15, 0.1));
    s.released();
    assert!(!s.moved(at(0.55, 0.1)) && s.text().is_none(), "a drag the window ended selects no more");
    // A drag kept for words still being read is dropped by a press elsewhere.
    let mut t = shown(2);
    let n2 = asks(&mut t, at(0.15, 0.1));
    t.moved(at(0.55, 0.1));
    t.released();
    t.clear();
    t.arrived(PageWordsReply::Words(words(2)), n2, &host());
    assert_eq!((t.text(), t.bands().len()), (None, 0), "the words of a cancelled drag mark nothing");
    // The button still down: Escape or a press elsewhere ends the drag, and no move, release or reply brings it back.
    let mut u = shown(2);
    let n = asks(&mut u, at(0.15, 0.1));
    u.moved(at(0.55, 0.1));
    u.clear();
    assert!(!u.moved(at(0.15, 0.55)));
    u.released();
    u.arrived(PageWordsReply::Words(words(2)), n, &host());
    assert_eq!((u.text(), u.selected()), (None, false));
    // The same with the words held: a move after the clear marks nothing.
    let mut v = shown(2);
    let n = asks(&mut v, at(0.15, 0.1));
    v.arrived(PageWordsReply::Words(words(2)), n, &host());
    v.moved(at(0.55, 0.1));
    v.clear();
    assert!(!v.moved(at(0.15, 0.55)));
    v.released();
    assert_eq!((v.text(), v.bands().len()), (None, 0));
}

/// A page turn, another file, another row or host drops the words and the selection and asks again; a zoom or a pan, which
/// only gives a new frame, keeps them and moves the marks with the page; what the page said is handed back so its notice goes.
#[test]
fn a_page_turn_drops_the_words_and_the_selection_and_a_zoom_keeps_them() {
    let mut s = shown(2);
    let n = asks(&mut s, at(0.15, 0.1));
    s.arrived(PageWordsReply::Words(words(2)), n, &host());
    s.moved(at(0.55, 0.1));
    s.released();
    let before = s.bands()[0];
    s.set_frame(rect(300.0, 150.0, 2000.0, 4000.0), rect(0.0, 0.0, 5000.0, 5000.0));
    let after = s.bands()[0];
    assert_eq!((s.text().as_deref(), after.w / before.w), (Some("alpha beta"), 2.0), "a zoom keeps them, the marks follow");
    for other in [key(3), Some(("host".into(), Some("row".into()), "files:b.pdf".into(), 2)), Some(("host".into(), Some("other".into()), "files:a.pdf".into(), 2)), Some(("elsewhere".into(), Some("row".into()), "files:a.pdf".into(), 2)), None] {
        let mut t = shown(2);
        let n = asks(&mut t, at(0.15, 0.1));
        t.arrived(PageWordsReply::Words(words(2)), n, &host());
        t.moved(at(0.55, 0.1));
        t.released();
        t.sync(other.clone());
        assert_eq!((t.text(), t.bands().len()), (None, 0), "{other:?}");
        t.sync(key(2));
        assert!(matches!(t.press(at(0.15, 0.1)), Some(Some(_))), "{other:?}: the page is asked for again");
    }
    // A fresh install of the same page (the file read again) drops the words and the frame, and keeps the page's key.
    s.set_frame(CANVAS, rect(0.0, 0.0, 5000.0, 5000.0));
    s.forget();
    assert_eq!((s.text(), s.key().cloned()), (None, key(2)));
    assert_eq!(s.press(at(0.15, 0.1)), None, "no frame until the next paint");
    // The notice of a page goes with it.
    let mut t = shown(2);
    let n = asks(&mut t, at(0.2, 0.1));
    assert_eq!(t.arrived(PageWordsReply::Words(PageWords { page: 2, cut: false, lines: vec![] }), n, &host()), Some(SCANNED));
    assert_eq!(t.sync(key(3)), Some(SCANNED), "the page turn hands the notice back");
    assert_eq!(t.sync(key(3)), None);
}

/// A page with no words, a refusal that says they cannot be read and a daemon that cannot say are said once, each in its
/// own words (a page with no words does not claim a scan), and a press asks no more; the daemon's line is said once for the
/// connection, not for each page; a refusal that is not "no words" says what happened and is asked again at the next press.
#[test]
fn a_page_with_no_words_says_so_once() {
    let empty = PageWords { page: 2, cut: false, lines: vec![] };
    let refused = |code: Option<&str>| PageWordsReply::Refused { code: code.map(str::to_string), error: "x".into() };
    let mut s = shown(2);
    let n = asks(&mut s, at(0.2, 0.1));
    assert_eq!(s.arrived(PageWordsReply::Words(empty), n, &host()), Some(SCANNED));
    assert_eq!(s.press(at(0.2, 0.1)), Some(None), "it is not asked again, so it is not said again");
    let mut s = shown(2);
    let n = asks(&mut s, at(0.2, 0.1));
    let said = s.arrived(refused(Some("no_words")), n, &host());
    assert!(said.is_some_and(|w| w.starts_with("The text on this page can't be read") && !w.contains("scanned")), "{said:?}");
    // The daemon's line: said once for the computer, on one page and the next; again after its connection came again.
    let mut s = shown(2);
    let n = asks(&mut s, at(0.2, 0.1));
    let old = s.arrived(PageWordsReply::Unsupported, n, &host());
    assert_eq!(old, Some("The computer that runs your sessions has an older Ship of Tools that cannot select text in a PDF; update it."));
    assert_eq!(s.press(at(0.2, 0.1)), Some(None));
    s.sync(key(3));
    let n = asks(&mut s, at(0.2, 0.1));
    assert_eq!(s.arrived(PageWordsReply::Unsupported, n, &host()), None, "said once for the connection");
    s.connected(&host());
    s.sync(key(4));
    let n = asks(&mut s, at(0.2, 0.1));
    assert_eq!(s.arrived(PageWordsReply::Unsupported, n, &host()), old, "said again for a new connection");
    let mut s = shown(2);
    let n = asks(&mut s, at(0.2, 0.1));
    assert!(s.arrived(refused(Some("kernel_unavailable")), n, &host()).is_some_and(|w| w.starts_with("Couldn't read")));
    assert!(s.press(at(0.2, 0.1)).is_some_and(|ask| ask.is_some()), "asked again");
}

/// A frame compares the page shown with the key held in place, whatever the state of `words`: the same page keeps, a changed
/// page or host does not, and an owner holding no key has nothing to drop.
#[test]
fn a_frame_compares_the_page_shown_with_the_key_held_in_every_state() {
    let (h, w) = (host(), Some("row".to_string()));
    let page = |n: u32| Some(("files:a.pdf", n));
    let fresh = PdfSelect::default();
    assert!(fresh.keeps(&h, &w, None) && fresh.keeps(&h, &w, page(2)), "no key held: nothing to drop");
    let empty = PageWords { page: 2, cut: false, lines: vec![] };
    let mut states = Vec::new();
    states.push(shown(2));
    let mut s = shown(2);
    asks(&mut s, at(0.2, 0.1));
    states.push(s);
    let mut s = shown(2);
    let n = asks(&mut s, at(0.15, 0.1));
    s.arrived(PageWordsReply::Words(words(2)), n, &host());
    states.push(s);
    for reply in [PageWordsReply::Words(empty), PageWordsReply::Unsupported, PageWordsReply::Refused { code: Some("no_words".into()), error: "x".into() }] {
        let mut s = shown(2);
        let n = asks(&mut s, at(0.2, 0.1));
        s.arrived(reply, n, &host());
        states.push(s);
    }
    for (i, s) in states.iter().enumerate() {
        assert!(s.keeps(&h, &w, page(2)), "state {i}: the same page");
        assert!(!s.keeps(&h, &w, page(3)) && !s.keeps(&h, &w, None) && !s.keeps(&h, &Some("other".into()), page(2))
            && !s.keeps(&"elsewhere".into(), &w, page(2)) && !s.keeps(&h, &w, Some(("files:b.pdf", 2))), "state {i}: another page");
    }
}
