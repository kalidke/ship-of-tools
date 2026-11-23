use super::*;

impl State {
    /// A notice is about the file shown: the same file installed again keeps it, another file's preview drops it.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_notice_stays_with_its_file(&mut self) -> anyhow::Result<()> {
        let reply = |node: &str, text: &str| PreviewReply {
            node_id: Some(node.into()), workspace_id: None, mime: "text/plain".into(), bytes: text.as_bytes().to_vec(),
            extras: None, generation: 0,
        };
        self.install_preview_reply(reply("files:a.txt", "first"));
        self.preview_says("about a");
        self.install_preview_reply(reply("files:a.txt", "first, again"));
        anyhow::ensure!(self.preview_notice.text() == Some("about a"), "the same file installed again dropped its notice");
        self.install_preview_reply(reply("files:b.txt", "second"));
        anyhow::ensure!(self.preview_notice.text().is_none(), "another file's preview left the notice about the file before");
        Ok(())
    }
}

impl State {
    /// `d` with no picture shown boxes the refusal over the preview and leaves the status line as it was; any other preview
    /// message is still on both.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_drawing_refusal_is_boxed_and_leaves_the_status_line(&mut self) -> anyhow::Result<()> {
        self.raster.hide();
        self.preview_notice.clear();
        self.status.set("held before the key");
        self.refuse_draw();
        let words = self.preview_notice.text().map(str::to_string);
        anyhow::ensure!(words.as_deref().is_some_and(|w| w.starts_with("Can't draw")), "the box says {words:?}");
        anyhow::ensure!(self.status.text() == "held before the key", "the status line says `{}`", self.status.text());
        self.preview_says("about the file");
        anyhow::ensure!(self.status.text() == "about the file" && self.preview_notice.text() == Some("about the file"),
            "another preview message was not on both");
        Ok(())
    }
}

impl State {
    /// A PDF page's preview reply makes that page the base on show: the page the reply names reaches the drawing's one
    /// decider through the raster's install, so `d` draws on it. A later page's reply is that page, an image file's
    /// reply a base with no page, and a text reply none.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_shown_pdf_page_is_the_drawing_base_by_its_page(&mut self) -> anyhow::Result<()> {
        use sot_protocol::ops::InkBase;
        let png = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png"));
        let reply = |node: &str, mime: &str, bytes: &[u8], page: Option<u32>| PreviewReply {
            node_id: Some(node.into()), workspace_id: None, mime: mime.into(), bytes: bytes.to_vec(),
            extras: page.map(|page| serde_json::json!({ "page": page, "page_count": 4 })), generation: 0,
        };
        let base = |node: &str, page| Some(InkBase::Figure { node_id: node.into(), page });
        self.ink.set_canvas(false);
        for page in [2, 3] {
            self.install_preview_reply(reply("files:doc.pdf", "image/png", png, Some(page)));
            let shown = self.shown_ink_base();
            anyhow::ensure!(shown == base("files:doc.pdf", Some(page)), "page {page} of a PDF shows the base {shown:?}");
        }
        self.install_preview_reply(reply("files:a.png", "image/png", png, None));
        let shown = self.shown_ink_base();
        anyhow::ensure!(shown == base("files:a.png", None), "an image file after a PDF shows the base {shown:?}");
        self.install_preview_reply(reply("files:a.txt", "text/plain", b"words", None));
        anyhow::ensure!(self.shown_ink_base().is_none(), "a text file shows the base {:?}", self.shown_ink_base());
        Ok(())
    }
}

impl State {
    /// A person drags over a PDF page, through the real `State`: a real frame gives the page its rectangles, the press sends
    /// the request for the page's words and keeps the press, the answer to that request marks the drag's words (the pass
    /// hands the renderer their bands), the copy chord resolves to the copy of those words and does it, the file read
    /// again drops them, and an answer to an older request marks nothing on the page asked again.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_drag_over_a_pdf_page_selects_marks_and_copies_its_words(&mut self) -> anyhow::Result<()> {
        use crate::net::transport::{OutgoingReq, PageWordsReply};
        use crate::ui::input::keybindings::Modifiers;
        use sot_protocol::ops::{PageWord, PageWords};
        let png = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png"));
        let page = |n: u32| PreviewReply {
            node_id: Some("files:doc.pdf".into()), workspace_id: None, mime: "image/png".into(), bytes: png.to_vec(),
            extras: Some(serde_json::json!({ "page": n, "page_count": 4 })), generation: 0,
        };
        let word = |l: u16, r: u16, text: &str| PageWord { left: l, top: 6554, right: r, bottom: 13107, text: text.into() };
        let words = || PageWords { page: 2, cut: false, lines: vec![vec![word(6554, 19661, "alpha"), word(32768, 45875, "beta")]] };
        let host = self.selected.host().clone();
        let (tx, mut sent) = tokio::sync::mpsc::unbounded_channel();
        self.conns = vec![(host.clone(), tx)];
        self.ink.set_canvas(false);
        self.install_preview_reply(page(2));
        self.redraw()?;
        let canvas = self.pdf_select.page_rect().ok_or_else(|| anyhow::anyhow!("a frame gave the page no rectangle"))?;
        let at = |x: f32, y: f32| (canvas.x + x * canvas.w, canvas.y + y * canvas.h);
        // The press asks for the words, once, and keeps the press; the drag runs before the answer.
        self.pointer.moved_to(at(0.15, 0.1));
        anyhow::ensure!(self.press_words(), "a press on the shown page was not the words'");
        anyhow::ensure!(self.focus_prompts.focus() == PaneFocus::Preview, "the press did not focus the preview");
        let Ok(OutgoingReq::PageWords { page: 2, serial, .. }) = sent.try_recv() else { anyhow::bail!("the press sent no request for page 2") };
        self.pointer.moved_to(at(0.55, 0.1));
        self.words_moved();
        self.on_page_words(host.clone(), serial, PageWordsReply::Words(words()));
        self.words_released();
        anyhow::ensure!(self.pdf_select.text().as_deref() == Some("alpha beta"), "the drag selected {:?}", self.pdf_select.text());
        // The pass hands the renderer the bands of the selected words.
        self.redraw()?;
        anyhow::ensure!(self.brushes.last_selection.len() == 1, "the frame marked {} bands", self.brushes.last_selection.len());
        // The copy chord, resolved as the window resolves a key in this context, reaches the preview's copy of the words.
        let (context, key) = (self.help_context(), winit::keyboard::Key::Character("c".into()));
        let primary = Modifiers { ctrl: !cfg!(target_os = "macos"), super_: cfg!(target_os = "macos"), ..Modifiers::default() };
        let action = self.bindings.resolve(&key, Some(&key), primary, context.consumes_text(), |a| context.allows(a));
        anyhow::ensure!(action == Some(Action::PreviewCopySelection), "the copy chord resolved to {action:?}");
        self.status.set("before the key");
        crate::ui::preview::keys::preview_action_key(self, action, false, 0);
        let said = self.status.text().to_string();
        anyhow::ensure!(said == "copied PDF text" || said.starts_with("copy failed"), "the copy key said `{said}`");
        // A press elsewhere (off the page, in no pane that takes it) clears the selection, as a text preview's goes.
        self.pointer.moved_to((1.0, 1.0));
        crate::ui::input::mouse::press_selections(self);
        self.words_released();
        anyhow::ensure!(self.pdf_select.text().is_none(), "a press elsewhere kept the selection");
        self.pointer.moved_to(at(0.15, 0.1));
        anyhow::ensure!(self.press_words(), "a press on the page again was not the words'");
        self.pointer.moved_to(at(0.55, 0.1));
        self.words_moved();
        self.words_released();
        anyhow::ensure!(self.pdf_select.text().as_deref() == Some("alpha beta"), "the words held were not selected again");
        // The file read again drops the words and the selection.
        self.install_preview_reply(page(2));
        anyhow::ensure!(self.pdf_select.text().is_none(), "the file read again kept the selection");
        // The page asked again: the answer to the first request marks nothing, the answer to the second does.
        self.redraw()?;
        self.pointer.moved_to(at(0.15, 0.1));
        anyhow::ensure!(self.press_words(), "a press after the install was not the words'");
        let Ok(OutgoingReq::PageWords { serial: again, .. }) = sent.try_recv() else { anyhow::bail!("the second press sent no request") };
        self.pointer.moved_to(at(0.55, 0.1));
        self.words_moved();
        self.on_page_words(host.clone(), serial, PageWordsReply::Words(words()));
        anyhow::ensure!(self.pdf_select.text().is_none(), "the first request's answer marked the page asked again");
        self.on_page_words(host, again, PageWordsReply::Words(words()));
        anyhow::ensure!(self.pdf_select.text().as_deref() == Some("alpha beta"), "the second request's answer was not taken");
        Ok(())
    }
}
