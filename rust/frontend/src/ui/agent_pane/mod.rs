//! The selected agent pane owns its target, clients, screen feed, input and presentation.

use super::*;
use std::time::{Duration, Instant};

mod attach;
mod input;
mod presentation;
mod replies;
pub(in crate::ui) mod keys;
mod screen;

use attach::*;
pub(in crate::ui) use presentation::{Presentation, PresentationCandidate};
pub(in crate::ui) use screen::*;
use presentation::{PaneFacts, PanePresentation};
use replies::DirectReply;
pub(in crate::ui) use input::WheelEffect;

/// The client operations the pane needs. Production keeps the capsule's worker and parser;
/// owner tests supply a scripted client without workers, sockets or processes.
pub(in crate::ui) trait PaneClient {
    fn screen(&self) -> &vt100::Screen;
    fn is_checkpointed(&self) -> bool;
    fn is_dead(&mut self) -> bool;
    fn status_line(&self) -> &str;
    fn pump(&mut self) -> bool;
    fn last_input_outcome(&self) -> Option<sot_capsule::attach_client::client::InputOutcome>;
    fn inputs_discarded(&self) -> usize;
    fn send_input(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16);
    fn reset_scroll(&mut self);
    fn scroll(&mut self, rows: i32);
    fn mouse_tracking_on(&self) -> bool;
    fn set_viewed(&self, viewed: bool);
}

impl PaneClient for PaneAttachClient {
    fn screen(&self) -> &vt100::Screen { self.screen() }
    fn is_checkpointed(&self) -> bool { self.is_checkpointed() }
    fn is_dead(&mut self) -> bool { self.is_dead() }
    fn status_line(&self) -> &str { self.status_line() }
    fn pump(&mut self) -> bool { self.pump() }
    fn last_input_outcome(&self) -> Option<sot_capsule::attach_client::client::InputOutcome> { self.last_input_outcome() }
    fn inputs_discarded(&self) -> usize { self.inputs_discarded() }
    fn send_input(&mut self, bytes: &[u8]) { self.send_input(bytes); }
    fn resize(&mut self, cols: u16, rows: u16) { self.resize(cols, rows); }
    fn reset_scroll(&mut self) { self.screen_mut().set_scrollback(0); }
    fn scroll(&mut self, rows: i32) { scroll_ring(self.screen_mut(), rows); }
    fn mouse_tracking_on(&self) -> bool { self.mouse_tracking_on() }
    fn set_viewed(&self, viewed: bool) { self.set_viewed(viewed); }
}

type Selection = ((u16, u16), (u16, u16));

/// Only reply admission constructs an installation target.
pub(in crate::ui) struct AdmittedAttach(WsKey);

/// No caller can mutate a client, pool, selection or receipt independently of pane operations.
pub(in crate::ui) struct AgentPane<C = PaneAttachClient> {
    bl_pane_target: Option<WsKey>,
    pty_size: Option<(u16, u16)>,
    llm_selection: Option<Selection>,
    llm_drag_active: bool,
    pane_attach_term: Option<C>,
    warm_attach: WarmAttachPool<C>,
    pane_hold: Option<HeldPaneScreen>,
    pane_dial_error: Option<String>,
    pane_attach_requested_at: Option<Instant>,
    pane_attach_started_at: Option<Instant>,
    pane_attach_episode_warnings: u32,
    pane_presentation: PanePresentation,
    pane_feed: PaneFeed,
    pane_inputs_discarded: usize,
}

impl<C: PaneClient> AgentPane<C> {
    pub(in crate::ui) fn new(target: Option<WsKey>) -> Self {
        Self {
            bl_pane_target: target, pty_size: None, llm_selection: None, llm_drag_active: false,
            pane_attach_term: None, warm_attach: WarmAttachPool::new(), pane_hold: None,
            pane_dial_error: None, pane_attach_requested_at: None, pane_attach_started_at: None,
            pane_attach_episode_warnings: 0, pane_presentation: PanePresentation::default(),
            pane_feed: PaneFeed::Pending, pane_inputs_discarded: 0,
        }
    }

    pub(in crate::ui) fn is_target(&self, key: &WsKey) -> bool { self.bl_pane_target.as_ref() == Some(key) }
    pub(in crate::ui) fn target(&self) -> Option<WsKey> { self.bl_pane_target.clone() }
    pub(in crate::ui) fn snapshot_target(&self) -> Option<String> { self.bl_pane_target.as_ref().map(|(_, s)| s.clone()) }
    pub(in crate::ui) fn restore_snapshot_target(&mut self, host: HostKey, target: Option<String>) {
        self.bl_pane_target = target.map(|s| (host, s));
    }
    pub(in crate::ui) fn size(&self) -> Option<(u16, u16)> { self.pty_size }
    fn open_size(&self) -> (u16, u16) { self.pty_size.unwrap_or((80, 24)) }
    pub(in crate::ui) fn selection(&self) -> Option<Selection> { self.llm_selection }
    #[cfg(test)]
    pub(in crate::ui) fn client_open_elapsed(&self) -> Option<Duration> {
        Some(self.pane_attach_started_at?.saturating_duration_since(self.pane_attach_requested_at?))
    }
    #[cfg(all(test, feature = "test-pane-timing"))]
    pub(in crate::ui) fn dead_client_status(&mut self) -> Option<String> {
        let client = self.pane_attach_term.as_mut()?;
        client.is_dead().then(|| client.status_line().to_string())
    }

    /// State has already admitted the kept-row outcome. Other pane state deliberately survives it.
    pub(in crate::ui) fn invalidate_kept_attachment(&mut self) {
        self.pane_attach_term = None;
        self.pane_inputs_discarded = 0;
        self.bl_pane_target = None;
        self.pane_feed = PaneFeed::Pending;
    }
}

#[cfg(test)]
mod owner_tests;
