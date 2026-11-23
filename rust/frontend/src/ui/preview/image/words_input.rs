//! A PDF page's text selection wired to the pointer, the copy key and the frame: what the person's hand does reaches
//! `PdfSelect` only through here, and which page is shown is asked of the raster alone (`pdf_page`).

use crate::net::transport::{OutgoingReq, PageWordsReply};
use crate::ui::preview::image::words_select::PageKey;
use crate::ui::*;

impl State {
    /// The PDF page on show, borrowed: its file and page, when a raster of a `.pdf` node (the plugin's rule: its name, in
    /// any case, ends in `.pdf`) with its page is shown with no edit, refusal or canvas over it, as `preview_shows` decides,
    /// without building it.
    fn pdf_shown(&self) -> Option<(&str, u32)> {
        let (node, page) = (self.raster.node()?, self.raster.page()?);
        let (editing, refused) = (self.editor.is_editing(), self.hello_refused.refused(self.selected.host()));
        let pdf = node.as_bytes().len() >= 4 && node.as_bytes()[node.len() - 4..].eq_ignore_ascii_case(b".pdf");
        (pdf && self.raster.is_shown() && !editing && !refused && !self.ink.canvas()).then_some((node, page))
    }

    /// The PDF page on show and whose it is.
    fn pdf_page(&self) -> Option<PageKey> {
        let (node, page) = self.pdf_shown()?;
        Some((self.selected.host().clone(), self.selected.workspace_id().clone(), node.to_owned(), page))
    }

    /// What was held for a page no longer shown is dropped, and the notice it put up goes with it.
    pub(in crate::ui) fn words_sync(&mut self) {
        let key = self.pdf_page();
        let said = self.pdf_select.sync(key);
        self.drop_notice(said);
    }

    /// Before a frame: the same, but the page shown is compared with the key held in place, and a key is built only when it
    /// has changed; a page nobody pressed on (no key held) costs nothing.
    pub(in crate::ui) fn words_frame(&mut self) {
        if !self.pdf_select.keeps(self.selected.host(), self.selected.workspace_id(), self.pdf_shown()) {
            self.words_sync();
        }
    }

    /// The notice that said `said` is about a page no longer shown.
    pub(in crate::ui) fn drop_notice(&mut self, said: Option<&str>) {
        if said.is_some() && self.preview_notice.text() == said {
            self.preview_notice.clear();
        }
    }

    /// A left press that selects on the PDF page (drawing is off, or it would have taken the press): focus moves to the
    /// preview, the words are asked for if they are not held, and a drag begins.
    pub(in crate::ui) fn press_words(&mut self) -> bool {
        self.words_sync();
        let Some(ask) = self.pdf_select.press(self.pointer.at_px()) else { return false };
        self.set_focus(PaneFocus::Preview);
        if let (Some(serial), Some((host, workspace_id, node_id, page))) = (ask, self.pdf_select.key().cloned()) {
            if let Err(e) = self.send(OutgoingReq::PageWords { node_id, workspace_id, page, serial }) {
                tracing::warn!(%e, "drop the words request — channel closed");
                self.pdf_select.arrived(PageWordsReply::Ended, serial, &host);
            }
        }
        self.window.request_redraw();
        true
    }

    pub(in crate::ui) fn words_moved(&mut self) {
        if self.pdf_select.moved(self.pointer.at_px()) {
            self.window.request_redraw();
        }
    }

    pub(in crate::ui) fn words_released(&mut self) { self.pdf_select.released(); }

    /// The reply to the request numbered `serial`: taken only if the page it was asked for is the one shown, which is settled
    /// first (a file installed since drops the request with its page), and the request is the one asked (the same page may
    /// have been asked again after the file changed). A reply that succeeds after a refusal takes the refusal's notice away.
    pub(in crate::ui) fn on_page_words(&mut self, host: HostKey, serial: u64, reply: PageWordsReply) {
        self.words_sync();
        let before = self.pdf_select.said();
        match (self.pdf_select.arrived(reply, serial, &host), self.pdf_select.said()) {
            (Some(words), _) => self.preview_says(words),
            (None, None) => self.drop_notice(before),
            (None, Some(_)) => {}
        }
        self.window.request_redraw();
    }

    /// The copy key over a PDF page: the selected words' text to the clipboard, and what to say of it. None when no words
    /// are selected, so the key does what it did.
    pub(in crate::ui) fn copy_words(&mut self) -> Option<String> {
        self.words_sync();
        copied(self.pdf_select.text(), |text| crate::ui::input::write_clipboard_text(text).map_err(|e| e.to_string()))
    }
}

/// `text` to the clipboard, and what to say of it; none, and the clipboard untouched, when there is no text.
fn copied(text: Option<String>, clipboard: impl FnOnce(String) -> std::result::Result<(), String>) -> Option<String> {
    Some(match clipboard(text?) {
        Ok(()) => "copied PDF text".into(),
        Err(error) => {
            tracing::warn!(%error, "failed to copy PDF text");
            format!("copy failed · {error}")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::copied;

    /// The copy key's text is the selected words' text alone: it reaches the clipboard whole and is said copied, a clipboard
    /// that refuses is said so, and with no words selected the key does what it did (no clipboard, nothing said).
    #[test]
    fn the_copy_key_copies_the_selected_words() {
        let mut got = Vec::new();
        assert_eq!(copied(Some("alpha beta\ngamma".into()), |t| { got.push(t); Ok(()) }).as_deref(), Some("copied PDF text"));
        assert_eq!(got, ["alpha beta\ngamma"]);
        assert_eq!(copied(Some("x".into()), |_| Err("no clipboard".into())).as_deref(), Some("copy failed · no clipboard"));
        assert_eq!(copied(None, |_| panic!("the clipboard was written with nothing selected")), None);
    }
}
