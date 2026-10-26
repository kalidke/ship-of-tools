//! Admit host-qualified replies before State performs a dial or applies a status effect.

use super::*;

pub(in crate::ui) enum DirectReply { Targetless, Ignore, Attach(AdmittedAttach) }

impl<C: PaneClient> AgentPane<C> {
    fn reply_selected(&self, event_host: &HostKey, active_host: &HostKey, target: Option<&str>) -> bool {
        event_host == active_host && target.is_some()
            && self.bl_pane_target.as_ref().is_some_and(|(h, s)| h == event_host && Some(s.as_str()) == target)
    }

    pub(in crate::ui) fn direct_reply(&self, event_host: &HostKey, active_host: &HostKey, target: Option<String>) -> DirectReply {
        let Some(target) = target else { return DirectReply::Targetless };
        if self.reply_selected(event_host, active_host, Some(&target)) && self.pane_attach_term.is_none() {
            DirectReply::Attach(AdmittedAttach((event_host.clone(), target)))
        } else { DirectReply::Ignore }
    }

    pub(in crate::ui) fn open_failed(&mut self, event_host: &HostKey, active_host: &HostKey, target: Option<&str>, error: &str) -> Option<String> {
        if !self.reply_selected(event_host, active_host, target) { return None; }
        let msg = format!("pty.open failed: {error}");
        self.pane_dial_error = Some(msg.clone());
        self.pane_feed = PaneFeed::Pending;
        Some(msg)
    }
}

impl State {
    pub(crate) fn on_pty_attach_direct(&mut self, event_host: HostKey, target: Option<String>) {
        match self.agent_pane.direct_reply(&event_host, self.selected.host(), target) {
            DirectReply::Targetless => tracing::warn!("pty.open refused attach_direct for a targetless (default) request — no row identity to correct or attach"),
            DirectReply::Ignore => {},
            DirectReply::Attach(target) => self.spawn_pane_attach_term(target),
        }
        self.window.request_redraw();
    }

    pub(crate) fn on_pty_open_failed(&mut self, event_host: HostKey, target: Option<String>, error: String) {
        if let Some(status) = self.agent_pane.open_failed(&event_host, self.selected.host(), target.as_deref(), &error) {
            tracing::warn!(?target, %error, "pty.open reply surfaced as a pane reason");
            self.status.set(status);
        }
        self.window.request_redraw();
    }
}
