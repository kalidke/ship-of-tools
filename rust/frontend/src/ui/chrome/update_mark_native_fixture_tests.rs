use super::*;
use crate::net::transport::{IncomingEvt, ResolvedDial};
use sot_protocol::{UpdatePhase, UpdateState};

impl State {
    /// The update's mark from the window's own event queue to a drawn frame. This computer's daemon says its update
    /// is ready and the next frame's bottom border holds the mark after the version stamp, its long words where the
    /// border has the room (a smaller text) and its short ones where it has not (a larger one); another computer's
    /// daemon's word is drawn nowhere; the local link's end takes the mark off the next frame and its return brings
    /// back only what the daemon says then; and the same events on a harness instance draw none. Before any daemon
    /// of this computer speaks, and after its link ends, the frame draws the state of the window's own update check.
    #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
    pub(in crate::ui) fn the_update_mark_is_drawn_for_this_computers_own_daemon(&mut self) -> Result<()> {
        let (here, hub) = ("<mark-here>".to_string(), "<mark-hub>".to_string());
        let (here_tx, _here_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hub_tx, _hub_rx) = tokio::sync::mpsc::unbounded_channel();
        let (events, receiver) = std::sync::mpsc::channel();
        let saved = (
            std::mem::replace(&mut self.conns, vec![(here.clone(), here_tx), (hub.clone(), hub_tx)]),
            std::mem::replace(&mut self.evt_rx, receiver),
            std::mem::replace(&mut self.update_mark, UpdateMark::new(false)),
        );
        let own = crate::selfupdate::state();
        let ready = |version: &str| UpdateState { version: version.into(), ..UpdateState::of(UpdatePhase::Ready) };
        crate::selfupdate::set_state_for_test(Some(ready("9.9.7")));
        let result = self.update_mark_run(&here, &hub, &events);
        crate::selfupdate::set_state_for_test(own);
        (self.conns, self.evt_rx, self.update_mark) = saved;
        result
    }

    fn update_mark_run(&mut self, here: &HostKey, hub: &HostKey, events: &std::sync::mpsc::Sender<(HostKey, IncomingEvt)>) -> Result<()> {
        let send = |host: &HostKey, evt: IncomingEvt| events.send((host.clone(), evt)).map_err(|_| anyhow::anyhow!("the fixture's event channel closed"));
        let hello = |resolved: ResolvedDial| IncomingEvt::Connected {
            session_id: "session".into(), revision: 0, host: None, project_root: None, proxy: false, resolved,
            backend_version: "test".into(), repl_running: Vec::new(), posts_as_person: true,
        };
        let says = |phase, version: &str| IncomingEvt::Update { state: UpdateState { version: version.into(), ..UpdateState::of(phase) } };
        let frame = |state: &mut Self| -> Result<String> {
            state.drain_events();
            state.redraw()?;
            Ok(state.update_mark_border())
        };

        // No daemon of this computer has spoken: the mark is the window's own check's (ready, 9.9.7).
        let alone = frame(self)?;
        anyhow::ensure!(alone.contains("9.9.7 ready"), "with no local daemon the frame drew {alone:?}");

        // The hub's daemon, reached over a relay, says ready: nothing of it is drawn.
        send(hub, hello(ResolvedDial::Relay("/nonexistent/relay.sock".into())))?;
        send(hub, says(UpdatePhase::Ready, "9.9.8"))?;
        let other = frame(self)?;
        anyhow::ensure!(other.contains("9.9.7 ready") && !other.contains("9.9.8"), "another computer's update is on the border: {other:?}");

        // This computer's daemon says preparing, then ready.
        send(here, hello(ResolvedDial::Local))?;
        send(here, says(UpdatePhase::Preparing, "9.9.9"))?;
        let preparing = frame(self)?;
        send(here, says(UpdatePhase::Ready, "9.9.9"))?;
        let ready = frame(self)?;
        let (stamp, mark) = (ready.find("fe "), ready.find("9.9.9 ready"));
        // The same state at two text sizes: the long words in the room a small text leaves, the short ones in less.
        let mult = self.canvas.text_scale_mult();
        self.apply_text_scale(0.5);
        let roomy = frame(self)?;
        self.apply_text_scale(1.2);
        let tight = frame(self)?;
        self.apply_text_scale(mult);
        println!("update-mark preparing={preparing:?} ready={ready:?} roomy={roomy:?} tight={tight:?}");
        anyhow::ensure!(roomy.contains("update 9.9.9 ready: quit keeping sessions, then start again"), "a border with room drew {roomy:?}");
        anyhow::ensure!(tight.contains("9.9.9 ready") && !tight.contains("applies"), "a border without room drew {tight:?}");
        anyhow::ensure!(preparing.contains("9.9.9 preparing") && !preparing.contains("9.9.7"), "the frame after preparing drew {preparing:?}");
        anyhow::ensure!(matches!((stamp, mark), (Some(stamp), Some(mark)) if stamp < mark), "the mark is not after the stamp: {ready:?}");

        // The local link ends: the mark goes with it, and a reconnect draws nothing until the daemon speaks.
        send(here, IncomingEvt::Disconnected { reason: "the daemon went away to apply".into(), refused: false })?;
        let gone = frame(self)?;
        send(here, hello(ResolvedDial::Local))?;
        let back = frame(self)?;
        anyhow::ensure!(!gone.contains("9.9.9") && !back.contains("9.9.9"), "the mark outlived its connection: {gone:?}, then {back:?}");
        anyhow::ensure!(gone.contains("9.9.7 ready"), "with the local daemon gone the frame drew {gone:?}, not the window's own state");
        send(here, says(UpdatePhase::None, ""))?;
        let none = frame(self)?;
        anyhow::ensure!(none.contains("fe ") && !none.contains("update"), "up to date drew {none:?}");

        // A harness instance, told the same, draws no mark.
        self.update_mark = UpdateMark::new(true);
        send(here, hello(ResolvedDial::Local))?;
        send(here, says(UpdatePhase::Ready, "9.9.9"))?;
        let harness = frame(self)?;
        anyhow::ensure!(harness.contains("fe ") && !harness.contains("9.9.9") && !harness.contains("9.9.7"), "a harness instance drew {harness:?}");
        Ok(())
    }

    /// The text on the chrome's bottom row as the last frame drew it, its runs in order.
    fn update_mark_border(&self) -> String {
        let bottom = f32::from(self.canvas.chrome().rows().saturating_sub(1));
        let runs: Vec<String> = self.canvas.chrome().project_lines(0.0, 0.0, 1.0, 1.0).into_iter().filter(|line| line.y == bottom).map(|line| line.text).collect();
        runs.join("|")
    }
}
