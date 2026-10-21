use super::*;
use crate::net::transport::IncomingEvt;

/// The drawer's entry `eval_id` in `log`: whether it is in flight, and its frames as text.
fn shown(log: &[ReplEntry], eval_id: u64) -> Option<(bool, Vec<String>)> {
    let entry = log.iter().find(|e| e.eval_id == eval_id)?;
    Some((entry.in_flight, entry.frames.iter().map(|f| format!("{f:?}")).collect()))
}

/// An entry of this window's own eval `eval_id`, in flight, having shown `text`.
fn running(eval_id: u64, text: &str) -> ReplEntry {
    ReplEntry {
        eval_id,
        code: format!("eval {eval_id}"),
        frames: vec![ReplFrame::Stdout { text: text.into() }],
        elapsed_ms: 0,
        in_flight: true,
        pkg_mode: false,
        origin: None,
    }
}

/// The one error frame the transport closes a run with when its connection ends (`PendingGuard`).
fn link_lost() -> ReplFrame {
    ReplFrame::Error {
        message: "the connection to the daemon ended before this eval's end".into(),
        stacktrace: Vec::new(),
    }
}

impl State {
    /// A run its connection's end closes keeps the output it had shown: the end's error frame is appended, never
    /// put in place of the frames, for an entry in the row on screen and for one in a parked row.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
    pub(in crate::ui) fn a_run_its_connections_end_closes_keeps_its_output(&mut self) -> Result<()> {
        let host: HostKey = "<repl-host>".to_string();
        self.active_host = host.clone();
        let (events, receiver) = std::sync::mpsc::channel();
        self.evt_rx = receiver;
        let (active, parked): (WsKey, WsKey) = (self.active_ws_key(), (host.clone(), "row-b".to_string()));
        self.repl_log = vec![running(41, "shown")];
        let mut pane = WorkspaceReplSnapshot::default();
        pane.repl_log = vec![running(42, "parked")];
        self.repl_rows.park(parked.clone(), pane);
        self.eval_id_workspace.insert((host.clone(), 41), active);
        self.eval_id_workspace.insert((host.clone(), 42), parked.clone());
        for eval_id in [41, 42] {
            let lost = IncomingEvt::ReplEvalDone { eval_id, elapsed_ms: 0, frames: vec![link_lost()] };
            events.send((host.clone(), lost)).map_err(|_| anyhow::anyhow!("the fixture's event channel closed"))?;
        }
        self.drain_events();
        let parked_log = self.repl_rows.parked.get(&parked).map(|p| p.repl_log.clone()).unwrap_or_default();
        let got = (shown(&self.repl_log, 41), shown(&parked_log, 42));
        println!("repl-drop-keeps-output: {got:?}");
        let stdout = |text: &str| format!("{:?}", ReplFrame::Stdout { text: text.into() });
        let kept = |text: &str| Some((false, vec![stdout(text), format!("{:?}", link_lost())]));
        anyhow::ensure!(got == (kept("shown"), kept("parked")), "(on screen, parked): {got:?}");
        Ok(())
    }

    /// A host's hello that names this window's runs still running reopens each entry of that host its link's end
    /// closed, on screen and in a parked row, saying the link came back, so the rest of each run reaches it and a
    /// Ctrl+C names it; an entry the hello does not name, and one of another host, stay closed.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
    pub(in crate::ui) fn a_returning_link_reopens_the_runs_still_running(&mut self) -> Result<()> {
        let (host, other): (HostKey, HostKey) = ("<repl-host>".to_string(), "<repl-other>".to_string());
        let (tx, _requests) = tokio::sync::mpsc::unbounded_channel();
        self.conns = vec![(host.clone(), tx)];
        self.active_host = host.clone();
        let (events, receiver) = std::sync::mpsc::channel();
        self.evt_rx = receiver;
        let send = |evt| {
            let closed = |_| anyhow::anyhow!("the fixture's event channel closed");
            events.send((host.clone(), evt)).map_err(closed)
        };
        let closed = |eval_id: u64, text: &str| ReplEntry {
            frames: vec![ReplFrame::Stdout { text: text.into() }, link_lost()],
            in_flight: false,
            ..running(eval_id, text)
        };
        let (parked, elsewhere): (WsKey, WsKey) = ((host.clone(), "row-b".into()), (other, "row-c".into()));
        self.repl_log = vec![closed(51, "a"), closed(53, "c")];
        for (row, eval_id) in [(&parked, 52), (&elsewhere, 54)] {
            let mut pane = WorkspaceReplSnapshot::default();
            pane.repl_log = vec![closed(eval_id, "b")];
            self.repl_rows.park(row.clone(), pane);
        }
        send(IncomingEvt::Connected {
            session_id: "session".into(),
            revision: 0,
            host: None,
            project_root: None,
            proxy: false,
            resolved: crate::net::transport::ResolvedDial::Local,
            backend_version: "test".into(),
            repl_running: vec![51, 52, 54, 99],
        })?;
        self.drain_events();
        let pane = |state: &State, row: &WsKey| {
            state.repl_rows.parked.get(row).map(|p| p.repl_log.clone()).unwrap_or_default()
        };
        let flight = |log: &[ReplEntry], eval_id| log.iter().find(|e| e.eval_id == eval_id).map(|e| e.in_flight);
        let (in_parked, in_elsewhere) = (pane(self, &parked), pane(self, &elsewhere));
        let on_screen = &self.repl_log;
        let reopened =
            [flight(on_screen, 51), flight(&in_parked, 52), flight(on_screen, 53), flight(&in_elsewhere, 54)];
        let interrupt = crate::ui::drawer::repl::log::in_flight_eval_ids(&self.repl_log);
        for eval_id in [51, 52] {
            let rest = ReplFrame::Stdout { text: "rest".into() };
            send(IncomingEvt::ReplFrameStreamed { eval_id, workspace_id: None, frame: rest })?;
            let done = ReplFrame::Done { eval_id, elapsed_ms: 5 };
            send(IncomingEvt::ReplFrameStreamed { eval_id, workspace_id: None, frame: done })?;
        }
        self.drain_events();
        let kinds = |log: &[ReplEntry], eval_id| {
            let entry = log.iter().find(|e| e.eval_id == eval_id)?;
            let kind = |f: &ReplFrame| format!("{f:?}").split(' ').next().unwrap_or_default().to_string();
            Some((entry.in_flight, entry.frames.iter().map(kind).collect::<Vec<_>>()))
        };
        let ended = (kinds(&self.repl_log, 51), kinds(&pane(self, &parked), 52));
        println!(
            "repl-link-back: in flight (51 on screen, 52 parked, 53 not named, 54 another host's) {reopened:?}, \
                 a Ctrl+C names {interrupt:?}, then {ended:?}"
        );
        anyhow::ensure!(reopened == [Some(true), Some(true), Some(false), Some(false)], "reopened: {reopened:?}");
        anyhow::ensure!(interrupt == [51], "a Ctrl+C on screen names {interrupt:?}");
        let rest = Some((false, ["Stdout", "Error", "Stderr", "Stdout"].map(String::from).to_vec()));
        anyhow::ensure!(ended == (rest.clone(), rest), "the rest of each run: {ended:?}");
        Ok(())
    }
}
