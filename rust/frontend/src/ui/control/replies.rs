//! The daemon's pushed events (`IncomingEvt::Event`), by op: workspace.changed, agent.message nav
//! envelopes, fe.command, preview.changed.

use crate::ui::*;

impl State {
    pub(crate) fn on_event(&mut self, event_host: HostKey, op: String, payload: serde_json::Value) {
        if op == sot_protocol::op::WORKSPACE_CHANGED {
            // Server pushed a workspace create/destroy; re-list so
            // the Sessions strip refreshes live (mirror the manual
            // poll). Idempotent if we triggered the change.
            // ADR 0042 L2a codex review, item E: ask the host
            // that actually pushed this event, not active_host
            // — a non-active host's workspace churn used to
            // silently re-query the WRONG connection.
            let _ =
                self.send_to(&event_host, crate::net::transport::OutgoingReq::WorkspaceList);
        } else if op == sot_protocol::op::AGENT_MESSAGE {
            // A session can drive this FE's nav by broadcasting a
            // `sot_ui` envelope as the message text. Filing mail
            // is the daemon's (`hub_link.rs`), never this
            // frontend's: anything that is not a nav command is
            // ignored here.
            let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(env) = parse_nav_envelope(text) {
                self.handle_nav_envelope(&event_host, &env);
            }
        } else if op == sot_protocol::op::FE_COMMAND {
            // ADR 0025 imperative FE command. The daemon broadcasts
            // to every connection (like agent.message); we parse,
            // self-filter on `target`, and route to an `FeCommand`
            // run through the existing `dispatch_fe_command` sink.
            match serde_json::from_value::<sot_protocol::ops::FeCommandEvt>(payload) {
                Ok(evt) => {
                    // route_fe_command applies the target filter
                    // (None = all FEs act; Some(self) = act,
                    // force-show eligible; Some(other) = ignore) and
                    // maps cmd→FeCommand (None = bad target / unknown
                    // cmd / missing arg). `urgent` rides on the
                    // mapped Preview/Reveal; the idle gate is applied
                    // in dispatch_fe_command, not here.
                    if let Some(cmd) = route_fe_command(&evt, &self_comm_handle()) {
                        tracing::info!(cmd = %evt.cmd, target = ?evt.target,
                            "fe.command: dispatching");
                        self.dispatch_fe_command(Some(&event_host), cmd);
                    } else {
                        tracing::debug!(cmd = %evt.cmd, target = ?evt.target,
                            "fe.command: ignored (target mismatch / unknown cmd / missing arg)");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "fe.command: malformed payload — ignoring");
                }
            }
        } else if op == sot_protocol::op::PREVIEW_CHANGED {
            self.on_preview_changed(event_host, payload);
        } else {
            tracing::debug!(%op, "evt");
        }
    }
}
