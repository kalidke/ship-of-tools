//! A PDF page's text selection wired to the pointer, the copy key and the frame: what the person's hand does reaches
//! `PdfSelect` only through here, and which page is shown is asked of the raster alone (`pdf_page`).

use crate::net::transport::{OutgoingReq, PageWordsReply};
use crate::ui::preview::image::words_select::PageKey;
use crate::ui::*;

impl State {
    /// The PDF page on show and whose it is: a raster of a `.pdf` node with its page, with no edit or refusal over it.
    fn pdf_page(&self) -> Option<PageKey> {
        let (node, page) = (self.raster.node()?, self.raster.page()?);
        let shown = self.preview_shows();
        (shown.png && !self.editor.is_editing() && node.to_lowercase().ends_with(".pdf"))
            .then(|| (self.selected.host().clone(), self.selected.workspace_id().clone(), node.to_owned(), page))
    }

    /// Before a frame and each pointer or key event: what was held for a page no longer shown is dropped.
    pub(in crate::ui) fn words_sync(&mut self) {
        let key = self.pdf_page();
        self.pdf_select.sync(key);
    }

    /// A left press that selects on the PDF page (drawing is off, or it would have taken the press): focus moves to the
    /// preview, the words are asked for if they are not held, and a drag begins.
    pub(in crate::ui) fn press_words(&mut self) -> bool {
        self.words_sync();
        let Some(ask) = self.pdf_select.press(self.pointer.at_px()) else { return false };
        self.set_focus(PaneFocus::Preview);
        if let (true, Some((_, workspace_id, node_id, page))) = (ask, self.pdf_select.key().cloned()) {
            if let Err(e) = self.send(OutgoingReq::PageWords { node_id: node_id.clone(), workspace_id: workspace_id.clone(), page }) {
                tracing::warn!(%e, "drop the words request — channel closed");
                self.pdf_select.arrived(PageWordsReply::Ended, &node_id);
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

    /// The reply to a words request, for the page it was asked of: one that is no longer the page shown is dropped.
    pub(in crate::ui) fn on_page_words(&mut self, host: HostKey, node_id: String, workspace_id: Option<String>, page: u32, reply: PageWordsReply) {
        self.words_sync();
        if self.pdf_select.key() != Some(&(host, workspace_id, node_id.clone(), page)) {
            return;
        }
        if let Some(words) = self.pdf_select.arrived(reply, &node_id) {
            self.preview_says(words);
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
