//! pty.open replies for the agent pane: attach direct to the row's capsule, or show why it failed.

use crate::ui::*;

impl State {
    pub(crate) fn on_pty_attach_direct(&mut self, event_host: HostKey, target: Option<String>) {
        // ADR 0042 slice L1b fix 1: this reply is about
        // `target` — the ORIGINAL request's own target,
        // carried end to end through `PendingKind::PtyOpen`
        // — never whatever `bl_pane_target` happens to be
        // when the (possibly stale/delayed) reply lands. A
        // user switch to a DIFFERENT row between the
        // `pty.open` send and this reply must not
        // misattribute the cache correction or the attach.
        //
        // ADR 0045 decision 1: unconditional on every
        // platform — a capsule row attaches through its own
        // daemon's `lane.connect` bridge (`spawn_pane_attach_term`
        // resolves the dial from `event_host` alone; no
        // state-dir is read from this reply any more).
        match target {
            None => {
                tracing::warn!(
                    "pty.open refused attach_direct for a targetless \
                     (default) request — no row identity to correct or attach"
                );
            }
            Some(target) => {
                // "Still selected" requires BOTH the target
                // name AND the replying host to match the
                // active pane — a same-named session on a
                // NON-active host answering late must not
                // be mistaken for the row the user is
                // actually looking at. bl_pane_target now
                // carries its own owner host (ADR 0042 L2a
                // item D), so this checks the full pair.
                let still_selected = event_host == self.active_host
                    && self.bl_pane_target.as_ref()
                        == Some(&(event_host.clone(), target.clone()));
                // Already corrected by an earlier reply
                // (or a fresh cache-hit switch) — a
                // duplicate/late refusal for the SAME
                // still-selected row must not spawn a
                // second client alongside the live one.
                let already_attached = self.pane_attach_term.is_some();
                if still_selected && !already_attached {
                    let (cols, rows) = self.pty_size.unwrap_or((80, 24));
                    if self.spawn_pane_attach_term(&event_host, &target, cols, rows) {
                        self.pane_feed = PaneFeed::Capsule;
                        self.status =
                            format!("attached BL → {target} (capsule, corrected)");
                    } else {
                        // spawn_pane_attach_term already set
                        // the failure status — stay pending
                        // (fix 2) rather than overwrite it.
                        self.pane_feed = PaneFeed::Pending;
                    }
                }
                // `!still_selected`: the user moved on —
                // the cache correction above is all this
                // reply does. `already_attached`:
                // `pane_feed` is already `Capsule` for
                // this target — nothing to change.
            }
        }
        self.window.request_redraw();
    }

    pub(crate) fn on_pty_open_failed(
        &mut self,
        event_host: HostKey,
        target: Option<String>,
        error: String,
    ) {
        // Mirrors `PtyAttachDirect`'s own `still_selected`
        // check: only the row this specific reply is ABOUT,
        // and only while it's still the one on screen, gets
        // the reason — a stale reply for a row the user has
        // since left must not retitle whatever they're
        // looking at now.
        let still_selected = target.is_some()
            && event_host == self.active_host
            && self.bl_pane_target.as_ref()
                == Some(&(event_host.clone(), target.clone().unwrap_or_default()));
        if still_selected {
            let msg = format!("pty.open failed: {error}");
            tracing::warn!(?target, %error, "pty.open reply surfaced as a pane reason");
            self.pane_dial_error = Some(msg.clone());
            self.status = msg;
            self.pane_feed = PaneFeed::Pending;
        }
        self.window.request_redraw();
    }
}
