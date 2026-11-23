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
        let deliver = |state: &mut Self, serial: u64, reply: PageWordsReply| state.on_page_words(host.clone(), serial, reply);
        // The press asks for the words, once, and keeps the press; the drag runs before the answer.
        self.pointer.moved_to(at(0.15, 0.1));
        anyhow::ensure!(self.press_words(), "a press on the shown page was not the words'");
        anyhow::ensure!(self.focus_prompts.focus() == PaneFocus::Preview, "the press did not focus the preview");
        let Ok(OutgoingReq::PageWords { page: 2, serial, .. }) = sent.try_recv() else { anyhow::bail!("the press sent no request for page 2") };
        self.pointer.moved_to(at(0.55, 0.1));
        self.words_moved();
        deliver(self, serial, PageWordsReply::Words(words()));
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
        deliver(self, serial, PageWordsReply::Words(words()));
        anyhow::ensure!(self.pdf_select.text().is_none(), "the first request's answer marked the page asked again");
        deliver(self, again, PageWordsReply::Words(words()));
        anyhow::ensure!(self.pdf_select.text().as_deref() == Some("alpha beta"), "the second request's answer was not taken");
        Ok(())
    }
}

/// What the fixtures below share: a PDF page's reply, a markdown file's, and a page's words.
mod pdf_fixture {
    use super::*;
    use sot_protocol::ops::{PageWord, PageWords};

    pub(in crate::ui) fn pdf(n: u32) -> PreviewReply {
        let png = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png"));
        PreviewReply {
            node_id: Some("files:doc.pdf".into()), workspace_id: None, mime: "image/png".into(), bytes: png.to_vec(),
            extras: Some(serde_json::json!({ "page": n, "page_count": 4 })), generation: 0,
        }
    }

    pub(in crate::ui) fn text(node: &str) -> PreviewReply {
        PreviewReply {
            node_id: Some(node.into()), workspace_id: None, mime: "text/plain".into(), bytes: b"notes".to_vec(), extras: None, generation: 0,
        }
    }

    pub(in crate::ui) fn words() -> PageWords {
        let word = |l: u16, r: u16, text: &str| PageWord { left: l, top: 6554, right: r, bottom: 13107, text: text.into() };
        PageWords { page: 2, cut: false, lines: vec![vec![word(6554, 19661, "alpha"), word(32768, 45875, "beta")]] }
    }

    impl State {
        /// A left press at `p` on the shown page: the number its request for the words went out under.
        pub(in crate::ui) fn pdf_press(&mut self, sent: &mut tokio::sync::mpsc::UnboundedReceiver<crate::net::transport::OutgoingReq>, p: (f32, f32)) -> anyhow::Result<u64> {
            self.pointer.moved_to(p);
            anyhow::ensure!(self.press_words(), "a press on the shown page was not the words'");
            // Other requests (a switch's reads) may be ahead of it on the connection.
            while let Ok(req) = sent.try_recv() {
                if let crate::net::transport::OutgoingReq::PageWords { serial, .. } = req {
                    return Ok(serial);
                }
            }
            anyhow::bail!("the press sent no request")
        }
    }
}
use pdf_fixture::{pdf, text, words};

impl State {
    /// A reply is taken only for the page shown, whatever kind it is, and a notice does not outlive what it said: another
    /// file installed between the press and the reply takes the reply and the once-per-connection line with it; a retry
    /// that succeeds takes the refusal's notice away before a page turn can carry it on; a box drawn over the page is not
    /// the page's to press on, and a press beside it is.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_pdf_pages_reply_and_notice_are_its_own(&mut self) -> anyhow::Result<()> {
        use crate::net::transport::PageWordsReply;
        let host = self.selected.host().clone();
        let (tx, mut sent) = tokio::sync::mpsc::unbounded_channel();
        self.conns = vec![(host.clone(), tx)];
        self.ink.set_canvas(false);
        let refused = || PageWordsReply::Refused { code: None, error: "x".into() };
        let empty = || PageWordsReply::Words(sot_protocol::ops::PageWords { page: 2, cut: false, lines: vec![] });
        // Another file installed between the press and the reply of each kind: nothing is said anywhere.
        for (kind, reply) in [("a refusal", refused()), ("an old daemon", PageWordsReply::Unsupported), ("no words", empty())] {
            self.install_preview_reply(pdf(2));
            self.redraw()?;
            let canvas = self.pdf_select.page_rect().ok_or_else(|| anyhow::anyhow!("a frame gave the page no rectangle"))?;
            let serial = self.pdf_press(&mut sent, (canvas.x + 0.2 * canvas.w, canvas.y + 0.1 * canvas.h))?;
            self.words_released();
            self.install_preview_reply(text("files:a.md"));
            self.status.set("held");
            self.on_page_words(host.clone(), serial, reply);
            anyhow::ensure!(self.preview_notice.text().is_none() && self.status.text() == "held",
                "{kind} for the PDF said `{:?}` over the file installed since", self.preview_notice.text());
        }
        // The line for an old daemon was not spent on that file: it is said at the next PDF page.
        self.install_preview_reply(pdf(2));
        self.redraw()?;
        let canvas = self.pdf_select.page_rect().ok_or_else(|| anyhow::anyhow!("a frame gave the page no rectangle"))?;
        let at = |x: f32, y: f32| (canvas.x + x * canvas.w, canvas.y + y * canvas.h);
        let serial = self.pdf_press(&mut sent, at(0.2, 0.1))?;
        self.words_released();
        self.on_page_words(host.clone(), serial, PageWordsReply::Unsupported);
        anyhow::ensure!(self.preview_notice.text().is_some_and(|t| t.starts_with("The computer that runs your sessions")),
            "the old daemon's line was not said at the PDF page: {:?}", self.preview_notice.text());
        // A refusal's notice, then the retry that succeeds: the notice goes at once, and the page turn finds none.
        self.install_preview_reply(pdf(2));
        self.redraw()?;
        let serial = self.pdf_press(&mut sent, at(0.2, 0.1))?;
        self.words_released();
        self.on_page_words(host.clone(), serial, refused());
        anyhow::ensure!(self.preview_notice.text().is_some_and(|t| t.starts_with("Couldn't read")), "the refusal said {:?}", self.preview_notice.text());
        // The page leaves the screen with no install (the canvas takes the pane): the frame drops what the page said.
        self.ink.set_canvas(true);
        self.redraw()?;
        anyhow::ensure!(self.preview_notice.text().is_none(), "the canvas over the page left {:?}", self.preview_notice.text());
        self.ink.set_canvas(false);
        self.redraw()?;
        let serial = self.pdf_press(&mut sent, at(0.2, 0.1))?;
        self.words_released();
        self.on_page_words(host.clone(), serial, refused());
        self.redraw()?;
        let cover = self.pdf_select.cover_rect().ok_or_else(|| anyhow::anyhow!("the notice drew no box over the page"))?;
        let points = || (0..20).flat_map(|i| (0..20).map(move |j| (i as f32 / 20.0 + 0.025, j as f32 / 20.0 + 0.025)));
        let beside = points().map(|(x, y)| at(x, y)).find(|&p| !crate::ui::preview::image::ink::inside(&cover, p))
            .ok_or_else(|| anyhow::anyhow!("the box covers the whole page"))?;
        let serial = self.pdf_press(&mut sent, beside)?;
        self.words_released();
        self.on_page_words(host.clone(), serial, PageWordsReply::Words(words()));
        anyhow::ensure!(self.preview_notice.text().is_none(), "a retry that succeeded left {:?}", self.preview_notice.text());
        self.install_preview_reply(pdf(3));
        anyhow::ensure!(self.preview_notice.text().is_none(), "the page turn found {:?}", self.preview_notice.text());
        // A box over the page: a press inside it is not the page's, a press beside it is.
        self.install_preview_reply(pdf(2));
        self.preview_says("a message over the page");
        self.redraw()?;
        let cover = self.pdf_select.cover_rect().ok_or_else(|| anyhow::anyhow!("the message drew no box over the page"))?;
        let covered = points().map(|(x, y)| at(x, y)).find(|&p| crate::ui::preview::image::ink::inside(&cover, p))
            .ok_or_else(|| anyhow::anyhow!("the box {cover:?} does not overlap the page {canvas:?}"))?;
        let beside = points().map(|(x, y)| at(x, y)).find(|&p| !crate::ui::preview::image::ink::inside(&cover, p))
            .ok_or_else(|| anyhow::anyhow!("the box covers the whole page"))?;
        self.pointer.moved_to(covered);
        anyhow::ensure!(!self.press_words() && sent.try_recv().is_err(), "a press under the box began a selection");
        self.pdf_press(&mut sent, beside)?;
        Ok(())
    }
}

impl State {
    /// A drag begun over a shown PDF page with its words held, its end at the page's `0.25` across: the page's rectangle.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_pdf_drag_is_begun(&mut self) -> anyhow::Result<ScreenRect> {
        use crate::net::transport::PageWordsReply;
        let host = self.selected.host().clone();
        let (tx, mut sent) = tokio::sync::mpsc::unbounded_channel();
        self.conns = vec![(host.clone(), tx)];
        self.ink.set_canvas(false);
        self.install_preview_reply(pdf(2));
        self.redraw()?;
        let canvas = self.pdf_select.page_rect().ok_or_else(|| anyhow::anyhow!("a frame gave the page no rectangle"))?;
        let serial = self.pdf_press(&mut sent, (canvas.x + 0.1 * canvas.w, canvas.y + 0.1 * canvas.h))?;
        self.on_page_words(host, serial, PageWordsReply::Words(words()));
        self.a_pdf_drag_moves_to(canvas, 0.25);
        Ok(canvas)
    }

    /// The pointer over the page at `x` across, its words' text then.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_pdf_drag_moves_to(&mut self, canvas: ScreenRect, x: f32) -> Option<String> {
        self.pointer.moved_to((canvas.x + x * canvas.w, canvas.y + 0.1 * canvas.h));
        self.words_moved();
        self.pdf_select.text()
    }
}

impl State {
    /// A request for a PDF page's words goes out and the person leaves for another session (one never visited, then one with
    /// its file cached): the page's rectangle goes with the page, and the request's refusal, arriving before any frame, shows
    /// no notice in the session entered, nor when the PDF's own session comes back.
    #[allow(dead_code, reason = "runs only inside a native State, from the premise overlays; no libtest body can construct one")]
    pub(in crate::ui) fn a_departing_pdf_request_says_nothing_in_the_session_entered(&mut self) -> anyhow::Result<()> {
        use crate::net::transport::PageWordsReply;
        let probe_row = |slug: &str, id: &str| {
            let mut row = crate::ui::nav::ws_info(slug, &format!("{slug}-session"));
            row.workspace_id = id.into();
            row
        };
        let a = "<host-words>".to_string();
        let (a_tx, mut sent) = tokio::sync::mpsc::unbounded_channel();
        self.conns = vec![(a.clone(), a_tx)];
        let mut default = crate::ui::nav::ws_info("default", "attach-default");
        default.is_default = true;
        default.agent = "none".into();
        self.on_workspaces(a.clone(), vec![default, probe_row("s1", "id-s1"), probe_row("s2", "id-s2"), probe_row("s3", "id-s3")]);
        self.ink.set_canvas(false);
        let refused = || PageWordsReply::Refused { code: None, error: "x".into() };
        // s2 is visited with a text file, then the PDF is shown in s1 and its words asked for; s3 has never been visited.
        self.switch_to_workspace(a.clone(), Some("s2".into()), None, false);
        self.install_preview_reply(text("files:b.txt"));
        self.switch_to_workspace(a.clone(), Some("s1".into()), None, false);
        self.install_preview_reply(pdf(2));
        self.redraw()?;
        let canvas = self.pdf_select.page_rect().ok_or_else(|| anyhow::anyhow!("a frame gave the page no rectangle"))?;
        let serial = self.pdf_press(&mut sent, (canvas.x + 0.2 * canvas.w, canvas.y + 0.1 * canvas.h))?;
        self.words_released();
        // A first visit: the departed page's rectangle goes with it, and its refusal says nothing here.
        self.switch_to_workspace(a.clone(), Some("s3".into()), None, false);
        anyhow::ensure!(self.pdf_select.page_rect().is_none(), "a first visit kept the departed page's rectangle");
        self.on_page_words(a.clone(), serial, refused());
        anyhow::ensure!(self.preview_notice.text().is_none(), "a first visit showed {:?}", self.preview_notice.text());
        // The PDF's own session come back, its page asked for again, and left for one with its text cached: the same.
        self.switch_to_workspace(a.clone(), Some("s1".into()), None, false);
        self.redraw()?;
        anyhow::ensure!(self.pdf_select.page_rect().is_some(), "the page came back with no rectangle after a frame");
        let serial = self.pdf_press(&mut sent, (canvas.x + 0.2 * canvas.w, canvas.y + 0.1 * canvas.h))?;
        self.words_released();
        self.switch_to_workspace(a.clone(), Some("s2".into()), None, false);
        anyhow::ensure!(self.pdf_select.page_rect().is_none(), "a return to a cached file kept the departed page's rectangle");
        self.on_page_words(a.clone(), serial, refused());
        anyhow::ensure!(self.preview_notice.text().is_none(), "a return to a cached text file showed {:?}", self.preview_notice.text());
        // And back at the page, a reply of the departed request is not the page's.
        self.switch_to_workspace(a.clone(), Some("s1".into()), None, false);
        self.on_page_words(a, serial, refused());
        anyhow::ensure!(self.preview_notice.text().is_none(), "the page came back with {:?}", self.preview_notice.text());
        Ok(())
    }
}
