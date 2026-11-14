use super::*;
use crate::net::transport::IncomingEvt;

/// The drawer's entry `eval_id` in `log`: whether it is in flight, and its frames as text.
fn shown(log: &[ReplEntry], eval_id: u64) -> Option<(bool, Vec<String>)> {
    let entry = log.iter().find(|e| e.eval_id == eval_id)?;
    Some((
        entry.in_flight,
        entry.frames.iter().map(|f| format!("{f:?}")).collect(),
    ))
}

/// Start through the same owner transition as the input adapter, and stream its initial output.
fn running(model: &mut ReplModel, row: &WsKey, text: &str) -> u64 {
    model.edit_draft(DraftEdit::Replace(format!("eval {text}")));
    let target = ReplTarget {
        host: row.0.clone(),
        workspace_id: Some(row.1.clone()),
        view_key: row.clone(),
    };
    let p = model.begin_eval(target).expect("nonempty fixture eval");
    let id = p.submission.eval_id();
    model.complete_submission(p.submission, Ok(()));
    model.apply_frame(
        row.0.clone(),
        id,
        row,
        row.clone(),
        ReplFrame::Stdout { text: text.into() },
    );
    id
}

fn closed(model: &mut ReplModel, row: &WsKey, text: &str) -> u64 {
    let id = running(model, row, text);
    model.apply_eval_completion(row.0.clone(), id, row, 0, vec![link_lost()]);
    id
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
    #[allow(
        dead_code,
        reason = "runs only inside a native State, from the window_minimized fixtures child"
    )]
    pub(in crate::ui) fn a_run_its_connections_end_closes_keeps_its_output(
        &mut self,
    ) -> Result<()> {
        let host: HostKey = "<repl-host>".to_string();
        self.selected.select(host.clone(), self.selected.workspace_id().clone());
        let (events, receiver) = std::sync::mpsc::channel();
        self.evt_rx = receiver;
        let (active, parked): (WsKey, WsKey) =
            (self.active_ws_key(), (host.clone(), "row-b".to_string()));
        self.repl_model = ReplModel::default();
        self.repl_model.restore_row(&parked);
        let parked_id = running(&mut self.repl_model, &parked, "parked");
        self.repl_model.capture_row(parked.clone());
        self.repl_model.restore_row(&active);
        let active_id = running(&mut self.repl_model, &active, "shown");
        for eval_id in [active_id, parked_id] {
            let lost = IncomingEvt::ReplEvalDone {
                eval_id,
                elapsed_ms: 0,
                frames: vec![link_lost()],
            };
            events
                .send((host.clone(), lost))
                .map_err(|_| anyhow::anyhow!("the fixture's event channel closed"))?;
        }
        self.drain_events();
        let parked_log = self.repl_model.parked_entries(&parked).unwrap_or_default();
        let got = (
            shown(self.repl_model.entries(), active_id),
            shown(parked_log, parked_id),
        );
        println!("repl-drop-keeps-output: {got:?}");
        let stdout = |text: &str| format!("{:?}", ReplFrame::Stdout { text: text.into() });
        let kept = |text: &str| Some((false, vec![stdout(text), format!("{:?}", link_lost())]));
        anyhow::ensure!(
            got == (kept("shown"), kept("parked")),
            "(on screen, parked): {got:?}"
        );
        Ok(())
    }

    /// A host's hello that names this window's runs still running reopens each entry of that host its link's end
    /// closed, on screen and in a parked row, saying the link came back, so the rest of each run reaches it and a
    /// Ctrl+C names it; an entry the hello does not name, and one of another host, stay closed.
    #[allow(
        dead_code,
        reason = "runs only inside a native State, from the window_minimized fixtures child"
    )]
    pub(in crate::ui) fn a_returning_link_reopens_the_runs_still_running(&mut self) -> Result<()> {
        let (host, other): (HostKey, HostKey) =
            ("<repl-host>".to_string(), "<repl-other>".to_string());
        let (tx, _requests) = tokio::sync::mpsc::unbounded_channel();
        self.conns = vec![(host.clone(), tx)];
        self.selected.select(host.clone(), self.selected.workspace_id().clone());
        let (events, receiver) = std::sync::mpsc::channel();
        self.evt_rx = receiver;
        let send = |evt| {
            let closed = |_| anyhow::anyhow!("the fixture's event channel closed");
            events.send((host.clone(), evt)).map_err(closed)
        };
        let (parked, elsewhere): (WsKey, WsKey) =
            ((host.clone(), "row-b".into()), (other, "row-c".into()));
        let active = self.active_ws_key();
        self.repl_model = ReplModel::default();
        self.repl_model.restore_row(&parked);
        let parked_id = closed(&mut self.repl_model, &parked, "b");
        self.repl_model.capture_row(parked.clone());
        self.repl_model.restore_row(&elsewhere);
        let other_id = closed(&mut self.repl_model, &elsewhere, "b");
        self.repl_model.capture_row(elsewhere.clone());
        self.repl_model.restore_row(&active);
        let active_id = closed(&mut self.repl_model, &active, "a");
        let unnamed_id = closed(&mut self.repl_model, &active, "c");
        send(IncomingEvt::Connected {
            session_id: "session".into(),
            revision: 0,
            host: None,
            project_root: None,
            proxy: false,
            resolved: crate::net::transport::ResolvedDial::Local,
            backend_version: "test".into(),
            repl_running: vec![active_id, parked_id, other_id, 99],
        })?;
        self.drain_events();
        let pane = |state: &State, row: &WsKey| {
            state
                .repl_model
                .parked_entries(row)
                .unwrap_or_default()
                .to_vec()
        };
        let flight = |log: &[ReplEntry], eval_id| {
            log.iter()
                .find(|e| e.eval_id == eval_id)
                .map(|e| e.in_flight)
        };
        let (in_parked, in_elsewhere) = (pane(self, &parked), pane(self, &elsewhere));
        let on_screen = self.repl_model.entries();
        let reopened = [
            flight(on_screen, active_id),
            flight(&in_parked, parked_id),
            flight(on_screen, unnamed_id),
            flight(&in_elsewhere, other_id),
        ];
        let interrupt = self.repl_model.in_flight_ids();
        for eval_id in [active_id, parked_id] {
            let rest = ReplFrame::Stdout {
                text: "rest".into(),
            };
            send(IncomingEvt::ReplFrameStreamed {
                eval_id,
                workspace_id: None,
                frame: rest,
            })?;
            let done = ReplFrame::Done {
                eval_id,
                elapsed_ms: 5,
            };
            send(IncomingEvt::ReplFrameStreamed {
                eval_id,
                workspace_id: None,
                frame: done,
            })?;
        }
        self.drain_events();
        let kinds = |log: &[ReplEntry], eval_id| {
            let entry = log.iter().find(|e| e.eval_id == eval_id)?;
            let kind = |f: &ReplFrame| {
                format!("{f:?}")
                    .split(' ')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            };
            Some((
                entry.in_flight,
                entry.frames.iter().map(kind).collect::<Vec<_>>(),
            ))
        };
        let ended = (
            kinds(self.repl_model.entries(), active_id),
            kinds(&pane(self, &parked), parked_id),
        );
        println!(
            "repl-link-back: in flight (on screen, parked, not named, another host's) {reopened:?}, \
                 a Ctrl+C names {interrupt:?}, then {ended:?}"
        );
        anyhow::ensure!(
            reopened == [Some(true), Some(true), Some(false), Some(false)],
            "reopened: {reopened:?}"
        );
        anyhow::ensure!(
            interrupt == [active_id],
            "a Ctrl+C on screen names {interrupt:?}"
        );
        let rest = Some((
            false,
            ["Stdout", "Error", "Stderr", "Stdout"]
                .map(String::from)
                .to_vec(),
        ));
        anyhow::ensure!(
            ended == (rest.clone(), rest),
            "the rest of each run: {ended:?}"
        );
        Ok(())
    }

    /// A wheel tick over the REPL's log rolls it a row at a time through the real frame: nothing moves at the event,
    /// each frame short of the last row asks for the next, the frame that shows the last row does not, and the rest
    /// is the tick's three rows.
    #[allow(
        dead_code,
        reason = "runs only inside a native State, from the window_minimized fixtures child"
    )]
    pub(in crate::ui) fn a_repl_wheel_tick_rolls_a_row_at_a_time(&mut self) -> Result<()> {
        // The fixture's window is a harness instance, which asks with zero; this one drives the motion.
        self.settings.smooth_scroll_ms = 150;
        self.repl_model = ReplModel::default();
        let row = self.active_ws_key();
        closed(&mut self.repl_model, &row, &(0..200).map(|n| format!("out {n}\n")).collect::<String>());
        if self.layout_help.drawer() != DrawerContent::Repl {
            let effect = self.layout_help.toggle_drawer(DrawerContent::Repl, self.focus_prompts.focus());
            effect.apply(self, None);
        }
        self.set_focus(PaneFocus::Repl);
        self.redraw()?;
        self.pointer.clear_residue();
        crate::ui::input::mouse::mouse_wheel(self, winit::keyboard::ModifiersState::empty(), winit::event::MouseScrollDelta::LineDelta(0.0, 1.0));
        anyhow::ensure!(self.repl_model.scroll() == 0, "the wheel moved the REPL at the event, not over frames");
        let (mut frames, begun) = (Vec::new(), std::time::Instant::now());
        while frames.last().map(|&(scroll, _)| scroll) != Some(3) {
            anyhow::ensure!(begun.elapsed() < std::time::Duration::from_secs(5), "the roll never came to rest: {frames:?}");
            self.pacer.clear_dirty();
            self.redraw()?;
            frames.push((self.repl_model.scroll(), self.pacer.is_dirty()));
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        println!("repl-roll: {frames:?}");
        anyhow::ensure!(frames.iter().any(|&(scroll, _)| scroll > 0 && scroll < 3), "no frame showed a row between: {frames:?}");
        let (last, before) = frames.split_last().expect("at least one frame");
        anyhow::ensure!(before.iter().all(|&(_, asked)| asked), "a frame short of the last row did not ask for the next: {frames:?}");
        anyhow::ensure!(!last.1, "the frame that showed the last row asked for another: {frames:?}");
        Ok(())
    }
}
