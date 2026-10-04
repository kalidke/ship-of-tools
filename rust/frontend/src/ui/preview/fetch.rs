//! The cursor-follow preview fetch: preview.get requests and their generations.

use crate::ui::*;

impl State {
    /// Whether the current preview source is a code shaper (`new_tokens` /
    /// `new_plain`) rather than markdown/image — i.e. one where line anchoring
    /// is meaningful. Keyed off the cached `preview_src` mime.
    fn preview_is_code(&self) -> bool {
        self.preview_src
            .as_ref()
            .map(|(m, _)| {
                m.starts_with("application/vnd.sot.tokens+json")
                    || (m.starts_with("text/") && m != "text/markdown" && m != "text/x-markdown")
            })
            .unwrap_or(false)
    }

    /// Switch-latency Phase 1: mint the next preview-slot request
    /// generation. Call this once per fired `preview.get`/
    /// `preview.set_scale` and stamp the result into the request — every
    /// `IncomingEvt::Preview` handler compares its echoed generation
    /// against `self.preview_req_gen`'s CURRENT value (read fresh at reply
    /// time, not the value captured here) to tell a stale reply from the
    /// latest one asked for. See the field doc for the full rationale.
    pub(in crate::ui) fn next_preview_gen(&mut self) -> u64 {
        self.preview_req_gen += 1;
        self.preview_req_gen
    }

    /// Same mechanism as `next_preview_gen`, for the concept/annotation slot.
    pub(in crate::ui) fn next_concept_gen(&mut self) -> u64 {
        self.concept_req_gen += 1;
        self.concept_req_gen
    }

    /// If the selected tree row's node id differs from the last one we
    /// asked for a preview of, fire a fresh `preview.get`. The Preview
    /// handler routes the response to the right pane based on mime.
    /// Modules-mode rows (no `files:` prefix) have no backend preview
    /// today; skip them rather than asking and getting an error back.
    /// C2: when a preview is pinned, cursor moves DON'T refresh the
    /// preview — the user is parked on the pinned node and the cursor
    /// is free to roam.
    pub(in crate::ui) fn maybe_fire_preview(&mut self) {
        if self.pinned_preview_node_id.is_some() {
            return;
        }
        // `--capture-preview` runs must show the requested node, full stop.
        // Restored nav state (the previous session's cursor) would otherwise
        // auto-fire its own preview and overwrite the captured one — the
        // root-row suppression at the dispatch site doesn't cover a restored
        // non-root cursor.
        if self.capture_preview_armed {
            return;
        }
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return;
        };
        // Files-mode rows are already keyed `files:<relpath>` — fire
        // directly. Modules-mode rows carry an absolute `file` on
        // their payload; we synthesize the `files:<relpath>` id from
        // the cached scan project_root so `preview.get` reuses the
        // same backend codepath.
        let node_id = if row.node.id.starts_with("files:") {
            Some(row.node.id.clone())
        } else if let Some(file) = row.node.payload.get("file").and_then(|v| v.as_str()) {
            if file.is_empty() {
                None
            } else {
                let rel = match &self.scan_project_root {
                    Some(root) if !root.is_empty() => file
                        .strip_prefix(root)
                        .map(|s| s.trim_start_matches(['/', '\\']).to_string())
                        .unwrap_or_else(|| file.to_string()),
                    _ => file.to_string(),
                };
                Some(format!("files:{rel}"))
            }
        } else {
            None
        };
        // Capture the row's definition line (modules-mode rows carry it on
        // `line`) so the Preview reply handler can anchor the code preview to
        // the item. Files-mode rows have no `line` → None → opens at the top.
        let anchor_line = row
            .node
            .payload
            .get("line")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let Some(id) = node_id else { return };
        // Blink guard: a freshly *driven* preview (fe-command / nav.preview) that
        // targeted a node not in the tree left the cursor parked on `id`'s row.
        // Don't fire `id`'s preview over the driven one while the cursor is still
        // parked there — that's the deep-path blink. The hold lifts as soon as
        // the cursor moves to a different row (then normal follow resumes).
        if let Some(held) = self.driven_preview_hold_cursor.clone() {
            if held == id {
                return;
            }
            self.driven_preview_hold_cursor = None;
        }
        if self.preview_node_id_fired.as_ref() == Some(&id) {
            // Same file already shown — no re-fetch needed. But items within a
            // module share a file, so the cursor moving between them must still
            // re-anchor the (already rendered) code preview to the new item.
            // Guard on a real change in target line so we don't re-anchor every
            // redraw and fight the user's manual scroll on a stable selection.
            if anchor_line != self.preview_anchored_to {
                if let Some(line) = anchor_line {
                    if line > 0 && self.preview_is_code() {
                        self.preview_scroll =
                            self.preview_md.anchor_scroll_for_def_line(line as usize);
                        self.window.request_redraw();
                    }
                }
                self.preview_anchored_to = anchor_line;
            }
            return;
        }
        // New target: drop any in-flight zoom re-raster so its reply can't
        // be mistaken for this fetch.
        self.preview_page_raster_pending = None;
        let (fit_w, fit_h) = self.preview_fit_px();
        let generation = self.next_preview_gen();
        if let Err(e) = self.send(crate::transport::OutgoingReq::PreviewGet {
            node_id: id.clone(),
            workspace_id: self.active_workspace_id.clone(),
            // Cursor-driven fetch always opens at page 1; the reply's
            // extras re-seed `preview_page` for the n/p transport.
            page: None,
            fit_w,
            fit_h,
            generation,
        }) {
            tracing::warn!(error = %e, %id, "drop preview.get request — channel closed");
            return;
        }
        self.preview_node_id_fired = Some(id);
        self.preview_anchor_line = anchor_line;
    }
}

/// Switch-latency Phase 1: the single stale-reply test shared by every
/// single-slot consumer (the preview pane, the concept/annotation slot).
/// A reply is only ever installed when BOTH hold:
///   - `generation == latest_generation` — this reply answers the MOST
///     RECENT request this session has fired for the slot. Generations are
///     minted per fired request (`State::next_preview_gen` /
///     `next_concept_gen`) and only ever increase, so an older one means a
///     newer request has since superseded it — the daemon answering
///     out-of-order (or simply slower) can never make an older answer look
///     newer than one already in flight.
///   - `event_host == active_host && reply_workspace == active_workspace`
///     — this reply's owner is still what the slot currently has active. A
///     generation match alone misses the one case where the ACTIVE (host,
///     workspace) changes without a fresh request being fired for the new
///     one (e.g. no in-flight preview existed there yet) — a stale reply
///     from the abandoned owner would otherwise still read as "latest".
///
/// Free function (not a `State` method) so it's unit-testable without
/// constructing the whole GPU/window state.
pub(in crate::ui) fn reply_is_current(
    generation: u64,
    latest_generation: u64,
    event_host: &HostKey,
    active_host: &HostKey,
    reply_workspace: &Option<String>,
    active_workspace: &Option<String>,
) -> bool {
    generation == latest_generation
        && event_host == active_host
        && reply_workspace == active_workspace
}

#[cfg(test)]
mod tests {
    use super::*;

    // Switch-latency Phase 1: `reply_is_current` is the whole stale-reply
    // guard for the preview pane and the concept/annotation slot — a
    // single free function shared by both `IncomingEvt` match arms, so one
    // set of cases covers both consumers.

    #[test]
    fn reply_is_current_accepts_the_latest_generation_for_the_active_owner() {
        assert!(reply_is_current(
            3,
            3,
            &"h".to_string(),
            &"h".to_string(),
            &Some("ws".to_string()),
            &Some("ws".to_string()),
        ));
        // `None` (the daemon-default workspace) matches itself too.
        assert!(reply_is_current(1, 1, &"h".to_string(), &"h".to_string(), &None, &None));
    }

    #[test]
    fn reply_is_current_drops_an_older_generation() {
        // A slower earlier request's reply landing after a newer one has
        // already been fired for the same slot — the core switch-latency
        // repro (an obsolete preview overwriting a newer cursor's target).
        assert!(!reply_is_current(
            1,
            3,
            &"h".to_string(),
            &"h".to_string(),
            &Some("ws".to_string()),
            &Some("ws".to_string()),
        ));
    }

    #[test]
    fn reply_is_current_drops_a_generation_ahead_of_the_latest_issued() {
        // Shouldn't happen (a reply can't answer a request this session
        // never sent), but the check is a strict equality, not `<=`, so a
        // forged/corrupt generation is rejected too rather than silently
        // becoming the new "latest".
        assert!(!reply_is_current(
            5,
            3,
            &"h".to_string(),
            &"h".to_string(),
            &Some("ws".to_string()),
            &Some("ws".to_string()),
        ));
    }

    #[test]
    fn reply_is_current_drops_a_non_active_host_even_at_the_latest_generation() {
        // `workspace_id: None` names "the default workspace" on EVERY
        // host, so the host leg of the owner check has to be independent
        // of the workspace leg — a stale reply from a host the session has
        // since switched away from must not be mistaken for the active one
        // just because both happen to be on their own default workspace.
        assert!(!reply_is_current(
            1,
            1,
            &"old-host".to_string(),
            &"active-host".to_string(),
            &None,
            &None,
        ));
    }

    #[test]
    fn reply_is_current_drops_a_non_active_workspace_even_at_the_latest_generation() {
        assert!(!reply_is_current(
            1,
            1,
            &"h".to_string(),
            &"h".to_string(),
            &Some("old-ws".to_string()),
            &Some("active-ws".to_string()),
        ));
    }
}
