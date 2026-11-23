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

/// A press on the page asks for its words once: not again while the request is out, nor once they are held; a request
/// that ended is asked again at the next press; a press off the page, or with no page shown, is not the words'.
#[test]
fn a_press_on_a_pdf_page_asks_once_for_its_words() {
    let mut s = shown(2);
    assert_eq!(s.press(at(0.2, 0.1)), Some(true));
    s.released();
    assert_eq!(s.press(at(0.2, 0.1)), Some(false), "the request is out");
    s.released();
    assert_eq!(s.arrived(PageWordsReply::Ended, "files:a.pdf"), None);
    assert_eq!(s.press(at(0.2, 0.1)), Some(true), "an ended request is asked again");
    assert_eq!(s.arrived(PageWordsReply::Words(words(2)), "files:a.pdf"), None);
    assert_eq!(s.text(), None, "words that come under a press with no motion select nothing");
    assert_eq!(s.press(at(0.2, 0.1)), Some(false), "the words are held");
    // Off the page's rectangle, off the visible image, and with no page shown.
    assert_eq!(s.press((5.0, 5.0)), None);
    s.set_frame(CANVAS, rect(0.0, 0.0, 300.0, 300.0));
    assert_eq!(s.press(at(0.5, 0.5)), None, "on the page but under the pane's edge");
    let mut none = PdfSelect::default();
    none.set_frame(CANVAS, rect(0.0, 0.0, 5000.0, 5000.0));
    assert_eq!(none.press(at(0.2, 0.1)), None);
}

/// A drag selects the words between its ends, marks them and gives their text, as the words arrive after the press too;
/// a press and release with no motion selects nothing and clears the last selection.
#[test]
fn a_drag_over_the_words_selects_marks_and_gives_their_text() {
    let mut s = shown(2);
    s.press(at(0.15, 0.1));
    assert!(s.moved(at(0.55, 0.1)), "motion before the words came is kept");
    assert_eq!((s.text(), s.bands().len()), (None, 0));
    s.arrived(PageWordsReply::Words(words(2)), "files:a.pdf");
    assert_eq!(s.text().as_deref(), Some("alpha beta"), "the drag's ends, from where the press and the pointer are now");
    assert_eq!(s.bands().len(), 1);
    s.moved(at(0.15, 0.55));
    s.released();
    assert_eq!(s.text().as_deref(), Some("alpha beta\ngamma"), "kept after the button is up");
    assert_eq!(s.bands().len(), 2);
    s.press(at(0.15, 0.1));
    assert_eq!(s.text(), None, "a press clears the last selection");
    s.released();
    assert_eq!((s.text(), s.bands().len()), (None, 0), "a press and release with no motion selects nothing");
    // The same drag run backward selects the same words.
    s.press(at(0.15, 0.55));
    s.moved(at(0.15, 0.1));
    s.released();
    assert_eq!(s.text().as_deref(), Some("alpha beta\ngamma"));
}

/// A page turn, another file, another row or host drops the words and the selection and asks again; a zoom or a pan, which
/// only gives a new frame, keeps them and moves the marks with the page.
#[test]
fn a_page_turn_drops_the_words_and_the_selection_and_a_zoom_keeps_them() {
    let mut s = shown(2);
    s.press(at(0.15, 0.1));
    s.arrived(PageWordsReply::Words(words(2)), "files:a.pdf");
    s.moved(at(0.55, 0.1));
    s.released();
    let before = s.bands()[0];
    s.set_frame(rect(300.0, 150.0, 2000.0, 4000.0), rect(0.0, 0.0, 5000.0, 5000.0));
    let after = s.bands()[0];
    assert_eq!((s.text().as_deref(), after.w / before.w), (Some("alpha beta"), 2.0), "a zoom keeps them, the marks follow");
    for other in [key(3), Some(("host".into(), Some("row".into()), "files:b.pdf".into(), 2)), Some(("host".into(), Some("other".into()), "files:a.pdf".into(), 2)), Some(("elsewhere".into(), Some("row".into()), "files:a.pdf".into(), 2)), None] {
        let mut t = shown(2);
        t.press(at(0.15, 0.1));
        t.arrived(PageWordsReply::Words(words(2)), "files:a.pdf");
        t.moved(at(0.55, 0.1));
        t.released();
        t.sync(other.clone());
        assert_eq!((t.text(), t.bands().len()), (None, 0), "{other:?}");
        t.sync(key(2));
        assert_eq!(t.press(at(0.15, 0.1)), Some(true), "{other:?}: the page is asked for again");
    }
    // A fresh install of the same page (the file read again) drops the words and keeps the page's key.
    s.set_frame(CANVAS, rect(0.0, 0.0, 5000.0, 5000.0));
    s.forget();
    assert_eq!((s.text(), s.key().cloned()), (None, key(2)));
    assert_eq!(s.press(at(0.15, 0.1)), Some(true));
}

/// A page with no words, a daemon that cannot say and a refusal say so once, in plain words, and a press asks no more; a
/// refusal that is not "no words" says what happened and is asked again at the next press.
#[test]
fn a_page_with_no_words_says_so_once() {
    let mut s = shown(2);
    s.press(at(0.2, 0.1));
    let empty = PageWords { page: 2, cut: false, lines: vec![] };
    let said = s.arrived(PageWordsReply::Words(empty), "files:a.pdf");
    assert_eq!(said, Some("No text to select on this page: a scanned page has none."));
    assert_eq!(s.press(at(0.2, 0.1)), Some(false), "it is not asked again, so it is not said again");
    let refused = |code: Option<&str>| PageWordsReply::Refused { code: code.map(str::to_string), error: "x".into() };
    let mut s = shown(2);
    s.press(at(0.2, 0.1));
    assert_eq!(s.arrived(refused(Some("no_words")), "files:a.pdf"), Some("No text to select on this page: a scanned page has none."));
    let mut s = shown(2);
    s.press(at(0.2, 0.1));
    assert_eq!(s.arrived(PageWordsReply::Unsupported, "files:a.pdf"), Some("This computer's daemon needs an update to select text in a PDF."));
    assert_eq!(s.press(at(0.2, 0.1)), Some(false));
    let mut s = shown(2);
    s.press(at(0.2, 0.1));
    assert!(s.arrived(refused(Some("kernel_unavailable")), "files:a.pdf").is_some_and(|w| w.starts_with("Couldn't read")));
    assert_eq!(s.press(at(0.2, 0.1)), Some(true), "asked again");
}
