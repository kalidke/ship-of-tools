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
                self.send_to(&event_host, crate::transport::OutgoingReq::WorkspaceList);
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
            // The daemon's file watcher reported a filesystem change
            // (create / modify / remove). On a create or remove the
            // affected directory's listing changed, so live-refresh
            // it in the Files nav tree — otherwise the pane shows a
            // stale listing until a manual re-nav (the reported bug).
            //
            // Acceptance is two-path — workspace-tag match on the
            // carried node_id, else path translation under the KNOWN
            // active root — see `resolve_preview_changed` for the
            // rationale (and the 2026-08-17 live forensics that
            // replaced the path-only scheme). Duplicate copies from
            // overlapping watchers re-fire the same idempotent
            // refresh; cheap.
            //
            // ADR 0042 L2a codex review, item E: `active_ws` /
            // `active_project_root()` below describe active_host's
            // OWN view -- there is no per-host parked preview
            // state to update for a non-active host, so a change
            // reported by any other host has nothing valid to
            // resolve against here. Without this gate a
            // coincidental node_id/path match against the
            // ACTIVE host's tag/root (e.g. two projects both
            // having "src/main.jl") could repaint the visible
            // pane with a non-active host's file content.
            if event_host != self.active_host {
                tracing::debug!(%event_host, active_host = %self.active_host,
                    "preview.changed from a non-active host — dropped");
                return;
            }
            let event_ws = payload.get("workspace_id").and_then(|v| v.as_str());
            let event_node = payload.get("node_id").and_then(|v| v.as_str());
            let event_path = payload.get("path").and_then(|v| v.as_str());
            let kind = payload.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let active_ws = self
                .active_workspace_id
                .as_deref()
                .or(self.default_workspace_slug.as_deref());
            let resolved = resolve_preview_changed(
                event_ws,
                event_node,
                event_path,
                active_ws,
                self.active_project_root(),
            );
            // Receipt log — the arm used to skip silently, which
            // made the live-refresh path undiagnosable from the FE
            // log (2026-08-17 forensics). The level splits on the
            // OUTCOME, not on arrival: a resolved event is rare and
            // actionable, an unresolved one is the bulk of a busy
            // host's traffic. The old comment here claimed
            // "debounced daemon-side, so info-level is low-volume";
            // measured on a laptop FE 2026-09-05 that is false —
            // 109 events in a 30 s idle window, 101 of them
            // unresolved, ~2.9 KB/s of formatted disk writes for
            // events that are then discarded. Keeping both outcomes
            // at info made the diagnostic log proportional to the
            // flood it exists to diagnose. Both paths still log
            // every field, so RUST_LOG=sot::gpu=debug restores the
            // 2026-08-17 forensic view verbatim.
            let Some(node_id) = resolved else {
                // Not ours to render (foreign workspace, or the
                // active root is unknown and the tag didn't match).
                tracing::debug!(
                    kind,
                    event_ws = ?event_ws,
                    path = ?event_path,
                    active_ws = ?active_ws,
                    "preview.changed dropped — not the active view"
                );
                return;
            };
            tracing::info!(
                kind,
                event_ws = ?event_ws,
                path = ?event_path,
                active_ws = ?active_ws,
                resolved = %node_id,
                "preview.changed received"
            );
            if kind == "created" || kind == "removed" {
                let parent = parent_files_node_id(&node_id);
                self.refresh_tree_dir_if_expanded(&parent);
            }
            // A change to the file the preview pane is currently
            // showing means its bytes changed underneath us — re-fire
            // `preview.get` so the pane reflects the new content.
            // BOTH kinds matter: an in-place rewrite arrives as
            // "modified", but atomic savers (write temp + rename
            // into place) deliver the SAME logical update as
            // "created" — the old modified-only gate left renamed-in
            // figures stale (the reported same-filename bug).
            // `preview_node_id_fired` is the source of truth for
            // "what the pane shows right now" (same anchor the
            // reconnect re-fetch uses); hold the current page so a
            // paginated preview doesn't snap back to page 1.
            if (kind == "modified" || kind == "created")
                && self.preview_node_id_fired.as_deref() == Some(node_id.as_str())
            {
                let (fit_w, fit_h) = self.preview_fit_px();
                let generation = self.next_preview_gen();
                let _ = self.send_to(
                    &event_host,
                    crate::transport::OutgoingReq::PreviewGet {
                        node_id: node_id.clone(),
                        workspace_id: self.active_workspace_id.clone(),
                        page: self.preview_page.map(|(p, _)| p),
                        fit_w,
                        fit_h,
                        generation,
                    },
                );
            }
        } else {
            tracing::debug!(%op, "evt");
        }
    }
}
