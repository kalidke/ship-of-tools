use super::*;
use std::time::{Duration, Instant};

type Requests = tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>;

fn page_replies(rx: &mut Requests) -> Vec<(String, String, String)> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|req| match req {
            OutgoingReq::PageReply { workspace_id, handle, line } => Some((workspace_id, handle, line)),
            _ => None,
        })
        .collect()
}

fn offer_command(handle: &str, line: &str) -> FeCommand {
    FeCommand::PageReply { workspace: "id-b".into(), handle: handle.into(), line: line.into() }
}

impl State {
    /// A reply offered by host B's daemon opens the bar; no other command and no second offer sends anything; and
    /// an Enter after the wait puts one `page.reply` with the line on B's connection and none on A's.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
    pub(in crate::ui) fn the_bars_enter_sends_page_reply_to_the_pages_host(&mut self) -> Result<()> {
        let (a, b) = ("<host-a>".to_string(), "<host-b>".to_string());
        let (a_tx, mut a_rx) = tokio::sync::mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = tokio::sync::mpsc::unbounded_channel();
        let saved_conns = std::mem::replace(&mut self.conns, vec![(a.clone(), a_tx), (b.clone(), b_tx)]);
        self.selected.select(a.clone(), None);
        let (mut row_a, mut row_b) = (ws_info("project", "stored-a"), ws_info("project", "stored-b"));
        (row_a.workspace_id, row_b.workspace_id) = ("id-a".into(), "id-b".into());
        self.on_workspaces(a.clone(), vec![row_a]);
        self.on_workspaces(b.clone(), vec![row_b]);
        while a_rx.try_recv().is_ok() {}
        while b_rx.try_recv().is_ok() {}
        self.reply_bar = ReplyBar::default();
        let line = "[sot-page] approve";

        // Every other command, and a reply for a row this window does not list, open no bar and send nothing.
        self.dispatch_fe_command(Some(&b), FeCommand::Notify { text: "hello".into(), level: None });
        self.dispatch_fe_command(Some(&b), FeCommand::PageReply { workspace: "id-none".into(), handle: "h".into(), line: line.into() });
        self.dispatch_fe_command(None, offer_command("repo__box", line));
        anyhow::ensure!(self.reply_bar.view(Instant::now()).is_none(), "a bar opened for a command that is not an offer");

        // The offer opens the bar on the window; the person has not answered, so nothing is sent.
        let offered = Instant::now();
        self.dispatch_fe_command(Some(&b), offer_command("repo__box", line));
        anyhow::ensure!(
            matches!(self.reply_sight(), Sight::Whole(_)),
            "the fixture window cannot show the whole bar (grid {:?})",
            self.canvas.grid_size().ok()
        );
        let view = self.reply_bar.view(Instant::now());
        anyhow::ensure!(view.as_ref().is_some_and(|v| v.line == line && v.to.contains("repo__box")), "no bar for the offer: {view:?}");

        // A second offer is dropped and said on the status line; the first stands.
        self.dispatch_fe_command(Some(&b), offer_command("other__box", "[sot-page] other"));
        anyhow::ensure!(self.status.text().starts_with("reply from a page dropped"), "status: {}", self.status.text());
        anyhow::ensure!(self.reply_bar.view(Instant::now()).is_some_and(|v| v.line == line), "the second offer replaced the first");

        let enter = BarKeyKind::Enter { repeat: false };
        let sent_so_far = |b_rx: &mut Requests, a_rx: &mut Requests| page_replies(b_rx).len() + page_replies(a_rx).len();

        // Laid out but never presented, however late the clock reads: nothing was shown, so Enter sends nothing.
        anyhow::ensure!(self.reply_bar_key(enter, offered + Duration::from_secs(10)), "the bar did not take an unseen Enter");
        anyhow::ensure!(sent_so_far(&mut b_rx, &mut a_rx) == 0, "an Enter sent a bar no frame had presented");

        // A presented frame starts the wait: an Enter straight after it sends nothing.
        self.redraw()?;
        anyhow::ensure!(self.reply_bar_key(enter, Instant::now()), "the bar did not take an Enter after its frame");
        anyhow::ensure!(sent_so_far(&mut b_rx, &mut a_rx) == 0, "an Enter straight after the first frame sent");

        // A quit question drawn over the bar takes every key and the bar none.
        self.focus_prompts.ask_quit(false);
        self.redraw()?;
        anyhow::ensure!(self.reply_bar.covered(), "the quit question did not cover the bar");
        anyhow::ensure!(!self.reply_bar_key(enter, Instant::now() + Duration::from_secs(10)), "the bar took a key under the dialog");
        // The dialog closes; a frame shows the bar again, and the wait starts over from that frame.
        self.focus_prompts.answer_quit(QuitPromptStep::Cancel);
        self.redraw()?;
        anyhow::ensure!(!self.reply_bar.covered(), "the bar stayed covered after the dialog closed");
        anyhow::ensure!(self.reply_bar_key(enter, Instant::now()), "the bar did not take an Enter after the dialog");
        anyhow::ensure!(sent_so_far(&mut b_rx, &mut a_rx) == 0, "an Enter straight after the dialog closed sent");

        // A second of presented frames later, Enter puts the one request, with the line, on the offering host's connection.
        std::thread::sleep(ARM + Duration::from_millis(150));
        self.redraw()?;
        anyhow::ensure!(self.reply_bar_key(enter, Instant::now()), "the bar did not take the armed Enter");
        let on_b = page_replies(&mut b_rx);
        anyhow::ensure!(
            on_b == [("id-b".to_string(), "repo__box".to_string(), line.to_string())] && page_replies(&mut a_rx).is_empty(),
            "the armed Enter sent {on_b:?}"
        );
        anyhow::ensure!(!self.reply_bar_key(enter, Instant::now()), "the sent bar still took a key");
        anyhow::ensure!(page_replies(&mut b_rx).is_empty(), "a second Enter sent again");

        self.reply_bar = ReplyBar::default();
        self.conns = saved_conns;
        Ok(())
    }
}
