//! What the window keeps of a person's text selection on a shown PDF page: whose page it is, the page's words once they
//! have come (`PageText`), the drag in progress and the words it selects. One field of `State`, with private fields; it
//! never takes `State`. It is keyed by the page shown (host, row, file, page): `sync` drops everything held for another,
//! which is how a page turn, another file and a session switch drop the words and the selection, while a zoom or a pan
//! (the same key, a new frame) keeps them. The words are asked for at a press, once, under a number the answer must
//! return: an answer is taken only for the request that was asked.
use super::ink::inside;
use super::words::PageText;
use crate::net::dial::HostKey;
use crate::net::transport::PageWordsReply;
use crate::ui::render::quad::ScreenRect;

/// The page shown: the host and row that show it, its file and its page.
pub(in crate::ui) type PageKey = (HostKey, Option<String>, String, u32);

const NO_TEXT: &str = "There is no text to select on this page.";
const UNREADABLE: &str = "The text on this page can't be read on the computer that runs this session, so it can't be selected.";
const OLD_DAEMON: &str = "The computer that runs your sessions has an older Ship of Tools that cannot select text in a PDF; update it.";
const NOT_READ: &str = "Couldn't read this page's text: press on it again to retry.";

#[derive(Default)]
enum Words {
    /// Not asked for (or its request ended): the next press asks.
    #[default]
    None,
    /// Asked for under this number, no answer yet.
    Asked(u64),
    Held(PageText),
    /// Answered that there are none, or that they cannot be read or the daemon cannot say: said, and not asked again.
    Said,
}

/// A press of the left button, its first point and its latest, in fractions of the page. `down` is false after the
/// release of a drag that moved while the words were still being read: its ends select them when they come.
struct Drag {
    anchor: (f32, f32),
    last: (f32, f32),
    moved: bool,
    down: bool,
}

#[derive(Default)]
pub(in crate::ui) struct PdfSelect {
    key: Option<PageKey>,
    /// The page's rectangle and the visible image rectangle, as the last frame drew them.
    frame: Option<(ScreenRect, ScreenRect)>,
    /// A box drawn over the preview (the help peek, a notice, a dialog): a press on it is not the page's.
    cover: Option<ScreenRect>,
    words: Words,
    drag: Option<Drag>,
    range: Option<(usize, usize)>,
    /// The number of the last request, which no reset takes back; and what this page's owner said, and the computers it
    /// said "update it" of since their connection came.
    serial: u64,
    said: Option<&'static str>,
    told: Vec<HostKey>,
}

impl PdfSelect {
    pub(in crate::ui) fn key(&self) -> Option<&PageKey> { self.key.as_ref() }

    pub(in crate::ui) fn set_frame(&mut self, canvas: ScreenRect, image: ScreenRect) { self.frame = Some((canvas, image)); }

    pub(in crate::ui) fn set_cover(&mut self, cover: Option<ScreenRect>) { self.cover = cover; }

    /// Nothing held, asked for or begun: a frame need not look which page is shown.
    pub(in crate::ui) fn idle(&self) -> bool { matches!(self.words, Words::None) && self.drag.is_none() }

    /// Words are selected: the copy key has them to copy.
    pub(in crate::ui) fn selected(&self) -> bool { matches!(self.words, Words::Held(_)) && self.range.is_some() }

    /// Everything is dropped but the numbering and what was told of a computer; what the page said comes back, so the
    /// notice that said it can go.
    fn reset(&mut self, key: Option<PageKey>, frame: Option<(ScreenRect, ScreenRect)>) -> Option<&'static str> {
        let said = self.said.take();
        *self = Self { key, frame, cover: self.cover, serial: self.serial, told: std::mem::take(&mut self.told), ..Self::default() };
        said
    }

    /// The page now shown: what was held for another page is dropped.
    pub(in crate::ui) fn sync(&mut self, key: Option<PageKey>) -> Option<&'static str> {
        if self.key == key { None } else { self.reset(key, self.frame) }
    }

    /// A fresh install of the page (not a zoom's re-raster) or a session's restore: the words, the selection and the frame
    /// the pointer maps through are the old ones'; the frame comes with the next paint.
    pub(in crate::ui) fn forget(&mut self) -> Option<&'static str> {
        let key = self.key.take();
        self.reset(key, None)
    }

    /// The window's own connection to `host` came up: it is told again if its daemon still cannot select.
    pub(in crate::ui) fn connected(&mut self, host: &HostKey) { self.told.retain(|h| h != host); }

    /// The page's rectangle, when it has a size.
    fn page(&self) -> Option<ScreenRect> { self.frame.map(|(canvas, _)| canvas).filter(|c| c.w > 0.0 && c.h > 0.0) }

    /// A left press at `p`: None when it is not on the page (a frame is known, `p` is in the page's rectangle and the
    /// image's, and under no box drawn over it), else `Some(Some(serial))` when the page's words must be asked for now,
    /// under that number. The press clears the last selection and begins a drag.
    pub(in crate::ui) fn press(&mut self, p: (f32, f32)) -> Option<Option<u64>> {
        let (canvas, image) = self.frame?;
        if self.key.is_none() || self.page().is_none() || !inside(&canvas, p) || !inside(&image, p) || self.cover.is_some_and(|c| inside(&c, p)) {
            return None;
        }
        let at = ((p.0 - canvas.x) / canvas.w, (p.1 - canvas.y) / canvas.h);
        self.drag = Some(Drag { anchor: at, last: at, moved: false, down: true });
        self.range = None;
        if !matches!(self.words, Words::None) {
            return Some(None);
        }
        self.serial += 1;
        self.words = Words::Asked(self.serial);
        Some(Some(self.serial))
    }

    /// Motion with the button down: the drag's end follows the pointer, and the words between its ends are selected. A
    /// move to the press's own position is no motion.
    pub(in crate::ui) fn moved(&mut self, p: (f32, f32)) -> bool {
        let (Some(canvas), Some(drag)) = (self.page(), self.drag.as_mut().filter(|d| d.down)) else { return false };
        let at = ((p.0 - canvas.x) / canvas.w, (p.1 - canvas.y) / canvas.h);
        if !drag.moved && at == drag.anchor {
            return false;
        }
        (drag.last, drag.moved) = (at, true);
        self.reselect();
        true
    }

    fn reselect(&mut self) {
        let (Words::Held(page), Some(drag), Some(canvas)) = (&self.words, &self.drag, self.page()) else { return };
        if drag.moved {
            self.range = page.select(drag.anchor, drag.last, canvas.w / canvas.h);
        }
    }

    /// The button is up, or the window lost its focus, or lost the release: the drag ends and its selection stays. A drag
    /// that moved while the words were still being read is kept for them. A press clears the last selection, so a press
    /// and release with no motion has selected nothing.
    pub(in crate::ui) fn released(&mut self) {
        let waiting = matches!(self.words, Words::Asked(_));
        self.drag = self.drag.take().filter(|d| d.moved && waiting).map(|d| Drag { down: false, ..d });
    }

    /// A press elsewhere, in another pane, or Escape: the selection goes as a text preview's does.
    pub(in crate::ui) fn clear(&mut self) {
        self.range = None;
        if self.drag.as_ref().is_some_and(|d| !d.down) {
            self.drag = None;
        }
    }

    /// The answer to the request numbered `serial`, from `host`: what to say to the person, if anything. An answer to
    /// any other request is dropped. A request that ended is asked again at the next press; a page with no words, one
    /// that cannot be read and a daemon that cannot say are said once (the daemon once for the connection).
    pub(in crate::ui) fn arrived(&mut self, reply: PageWordsReply, serial: u64, host: &HostKey) -> Option<&'static str> {
        if !matches!(self.words, Words::Asked(n) if n == serial) {
            return None;
        }
        let (words, said) = match reply {
            PageWordsReply::Words(w) if w.lines.is_empty() => (Words::Said, Some(NO_TEXT)),
            PageWordsReply::Words(w) => (Words::Held(PageText::new(w)), None),
            PageWordsReply::Unsupported if self.told.contains(host) => (Words::Said, None),
            PageWordsReply::Unsupported => {
                self.told.push(host.clone());
                (Words::Said, Some(OLD_DAEMON))
            }
            PageWordsReply::Refused { code, .. } if code.as_deref() == Some("no_words") => (Words::Said, Some(UNREADABLE)),
            PageWordsReply::Refused { .. } => (Words::None, Some(NOT_READ)),
            PageWordsReply::Ended => (Words::None, None),
        };
        self.words = words;
        self.reselect();
        if self.drag.as_ref().is_some_and(|d| !d.down) {
            self.drag = None;
        }
        self.said = said;
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

    /// The page's rectangle, for a test that presses on it.
    #[cfg(test)]
    pub(in crate::ui) fn page_rect(&self) -> Option<ScreenRect> { self.page() }
}

#[cfg(test)]
#[path = "words_select_tests.rs"]
mod tests;
