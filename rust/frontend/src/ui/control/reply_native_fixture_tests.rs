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
            self.reply_sight(offered) == Sight::Whole,
            "the fixture window cannot show the whole bar (grid {:?})",
            self.canvas.grid_size().ok()
        );
        let view = self.reply_bar.view(Instant::now());
        anyhow::ensure!(view.as_ref().is_some_and(|v| v.line == line && v.to.contains("repo__box")), "no bar for the offer: {view:?}");

        // A second offer is dropped and said on the status line; the first stands.
        self.dispatch_fe_command(Some(&b), offer_command("other__box", "[sot-page] other"));
        anyhow::ensure!(self.status.text().starts_with("reply from a page dropped"), "status: {}", self.status.text());
        anyhow::ensure!(self.reply_bar.view(Instant::now()).is_some_and(|v| v.line == line), "the second offer replaced the first");

        // Enter before the wait sends nothing: the clock reads the offer's own moment, before the bar was first in view.
        anyhow::ensure!(self.reply_bar_key(BarKeyKind::Enter { repeat: false }, offered), "the bar did not take an early Enter");
        anyhow::ensure!(page_replies(&mut b_rx).is_empty() && page_replies(&mut a_rx).is_empty(), "an early Enter sent");

        // Enter after the wait puts the one request, with the line, on the offering host's connection.
        let later = offered + Duration::from_secs(5);
        self.observe_reply_bar(later);
        anyhow::ensure!(self.reply_bar_key(BarKeyKind::Enter { repeat: false }, later), "the bar did not take the armed Enter");
        let on_b = page_replies(&mut b_rx);
        anyhow::ensure!(
            on_b == [("id-b".to_string(), "repo__box".to_string(), line.to_string())] && page_replies(&mut a_rx).is_empty(),
            "the armed Enter sent {on_b:?}"
        );
        anyhow::ensure!(!self.reply_bar_key(BarKeyKind::Enter { repeat: false }, later), "the sent bar still took a key");
        anyhow::ensure!(page_replies(&mut b_rx).is_empty(), "a second Enter sent again");

        self.reply_bar = ReplyBar::default();
        self.conns = saved_conns;
        Ok(())
    }
}
