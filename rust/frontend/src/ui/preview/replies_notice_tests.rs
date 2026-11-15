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
