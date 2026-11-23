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
