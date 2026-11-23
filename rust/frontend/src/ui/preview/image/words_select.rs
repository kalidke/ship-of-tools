//! What the window keeps of a person's text selection on a shown PDF page: whose page it is, the page's words once they
//! have come (`PageText`), the drag in progress and the words it selects. One field of `State`, with private fields; it
//! never takes `State`. It is keyed by the page shown (host, row, file, page): `sync` drops everything held for another,
//! which is how a page turn, another file and a session switch drop the words and the selection, while a zoom or a pan
//! (the same key, a new frame) keeps them. The words are asked for at a press, once.
use super::words::PageText;
use crate::net::dial::HostKey;
use crate::net::transport::PageWordsReply;
use crate::ui::render::quad::ScreenRect;

/// The page shown: the host and row that show it, its file and its page.
pub(in crate::ui) type PageKey = (HostKey, Option<String>, String, u32);

const NO_TEXT: &str = "No text to select on this page: a scanned page has none.";
const OLD_DAEMON: &str = "This computer's daemon needs an update to select text in a PDF.";
const NOT_READ: &str = "Couldn't read this page's text: press on it again to retry.";

#[derive(Default)]
enum Words {
    /// Not asked for (or its request ended): the next press asks.
    #[default]
    None,
    /// Asked for, no answer yet.
    Asked,
    Held(PageText),
    /// Answered that there are none (or that the daemon cannot say): said once, and not asked again.
    Said,
}

/// A press of the left button, its first point and its latest, in fractions of the page.
struct Drag {
    anchor: (f32, f32),
    last: (f32, f32),
    moved: bool,
}

#[derive(Default)]
pub(in crate::ui) struct PdfSelect {
    key: Option<PageKey>,
    /// The page's rectangle and the visible image rectangle, as the last frame drew them.
    frame: Option<(ScreenRect, ScreenRect)>,
    words: Words,
    drag: Option<Drag>,
    range: Option<(usize, usize)>,
}

fn holds(r: &ScreenRect, p: (f32, f32)) -> bool {
    p.0 >= r.x && p.0 <= r.x + r.w && p.1 >= r.y && p.1 <= r.y + r.h
}

impl PdfSelect {
    pub(in crate::ui) fn key(&self) -> Option<&PageKey> { self.key.as_ref() }

    pub(in crate::ui) fn set_frame(&mut self, canvas: ScreenRect, image: ScreenRect) { self.frame = Some((canvas, image)); }

    /// The page now shown: what was held for another page is dropped.
    pub(in crate::ui) fn sync(&mut self, key: Option<PageKey>) {
        if self.key != key {
            *self = Self { key, frame: self.frame, ..Self::default() };
        }
    }

    /// A fresh install of the page (not a zoom's re-raster): its words and selection are the old file's.
    pub(in crate::ui) fn forget(&mut self) { *self = Self { key: self.key.take(), frame: self.frame, ..Self::default() }; }

    /// The page's rectangle, when it has a size.
    fn page(&self) -> Option<ScreenRect> { self.frame.map(|(canvas, _)| canvas).filter(|c| c.w > 0.0 && c.h > 0.0) }

    /// A left press at `p`: None when it is not on the page (inside the page's rectangle and the image's), else whether
    /// the page's words must be asked for now. The press clears the last selection and begins a drag.
    pub(in crate::ui) fn press(&mut self, p: (f32, f32)) -> Option<bool> {
        let (canvas, image) = self.frame?;
        if self.key.is_none() || self.page().is_none() || !holds(&canvas, p) || !holds(&image, p) {
            return None;
        }
        let at = ((p.0 - canvas.x) / canvas.w, (p.1 - canvas.y) / canvas.h);
        self.drag = Some(Drag { anchor: at, last: at, moved: false });
        self.range = None;
        let ask = matches!(self.words, Words::None);
        if ask {
            self.words = Words::Asked;
        }
        Some(ask)
    }

    /// Motion with the button down: the drag's end follows the pointer, and the words between its ends are selected.
    pub(in crate::ui) fn moved(&mut self, p: (f32, f32)) -> bool {
        let (Some(canvas), Some(drag)) = (self.page(), self.drag.as_mut()) else { return false };
        drag.last = ((p.0 - canvas.x) / canvas.w, (p.1 - canvas.y) / canvas.h);
        drag.moved = true;
        self.reselect();
        true
    }

    fn reselect(&mut self) {
        let (Words::Held(page), Some(drag), Some(canvas)) = (&self.words, &self.drag, self.page()) else { return };
        if drag.moved {
            self.range = page.select(drag.anchor, drag.last, canvas.w / canvas.h);
        }
    }

    /// The button is up: the drag ends and its selection stays. A press clears the last selection, so a press and release
    /// with no motion has selected nothing.
    pub(in crate::ui) fn released(&mut self) { self.drag = None; }

    /// The answer to the request: what to say to the person, if anything. A request that ended is asked again at the
    /// next press; a page with no words, and a daemon that cannot say, are said once.
    pub(in crate::ui) fn arrived(&mut self, reply: PageWordsReply, node: &str) -> Option<&'static str> {
        let (words, said) = match reply {
            PageWordsReply::Words(w) if w.lines.is_empty() => (Words::Said, Some(NO_TEXT)),
            PageWordsReply::Words(w) => (Words::Held(PageText::new(node.to_owned(), w)), None),
            PageWordsReply::Unsupported => (Words::Said, Some(OLD_DAEMON)),
            PageWordsReply::Refused { code, .. } if code.as_deref() == Some("no_words") => (Words::Said, Some(NO_TEXT)),
            PageWordsReply::Refused { .. } => (Words::None, Some(NOT_READ)),
            PageWordsReply::Ended => (Words::None, None),
        };
        self.words = words;
        self.reselect();
        said
    }

    /// The selected words' text.
    pub(in crate::ui) fn text(&self) -> Option<String> {
        match (&self.words, self.range) {
            (Words::Held(page), Some(range)) => page.text(range),
            _ => None,
        }
    }

    /// The marks for the selected words, on this frame's rectangles.
    pub(in crate::ui) fn bands(&self) -> Vec<ScreenRect> {
        match (&self.words, self.range, self.frame) {
            (Words::Held(page), Some(range), Some((canvas, image))) => page.bands(range, canvas, image),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "words_select_tests.rs"]
mod tests;
