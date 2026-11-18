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
