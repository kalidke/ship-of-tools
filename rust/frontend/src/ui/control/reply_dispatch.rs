//! The window's side of the reply bar: an offer opens it, what is drawn on top decides who gets a key, and an armed
//! Enter sends the one `page.reply`.

use super::*;
use std::time::Instant;
use winit::keyboard::ModifiersState;

/// A key as the bar reads it: the logical key, a plain Enter or Esc only (a chord is another key).
pub(in crate::ui) fn bar_key_kind(logical: &Key, repeat: bool, modifiers: ModifiersState) -> BarKeyKind {
    match logical {
        Key::Named(NamedKey::Enter) if modifiers.is_empty() => BarKeyKind::Enter { repeat },
        Key::Named(NamedKey::Escape) if modifiers.is_empty() => BarKeyKind::Esc,
        _ => BarKeyKind::Other,
    }
}

/// What a close dialog and the bar's panel make of the bar's sight: covered by a dialog, cut off, or whole at the
/// panel's rectangle (`panel` is `None` when no bar is open or the grid is unknown, which is not whole).
pub(in crate::ui) fn sight_of(dialog_open: bool, panel: Option<&CloseDialog>) -> Sight {
    match panel {
        _ if dialog_open => Sight::Covered,
        Some(panel) if !panel.clipped => Sight::Whole(panel.panel),
        _ => Sight::Cut,
    }
}

impl State {
    /// A reply a page offered, which came from the daemon `host`: the bar opens for a row this window lists on that
    /// host. It never sends: only the bar's Enter does (`reply_bar_key`).
    pub(in crate::ui) fn offer_page_reply(&mut self, host: &HostKey, workspace: &str, handle: String, line: String) {
        let name = match self.row_directory.resolve(host, workspace) {
            Ok(row) if row.identity().workspace_id == workspace => {
                let slug = row.row_key().1.as_str();
                self.row_presentation.label(host, slug).unwrap_or(slug).to_string()
            }
            _ => {
                tracing::warn!(%handle, "page reply dropped: the row is not listed");
                self.status.set(format!("reply from a page dropped \u{b7} {handle} is not a row this window lists"));
                self.status.hold();
                self.window.request_redraw();
                return;
            }
        };
        let offer = ReplyOffer { host: host.clone(), workspace_id: workspace.to_string(), handle: handle.clone(), name, line };
        // Covered until laid out, and shown only by a presented frame, so a bar cannot arm before it has been seen.
        match self.reply_bar.offer(offer, Sight::Covered) {
            Ok(()) => {
                tracing::info!(%handle, "page reply: bar opened");
                self.observe_reply_bar();
                self.window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
            }
            Err(refused) => {
                let why = match &refused {
                    OfferRefused::Busy => "another reply waits for your answer".to_string(),
                    OfferRefused::Line(reason) => reason.clone(),
                };
                tracing::warn!(%handle, %why, "page reply dropped");
                self.status.set(format!("reply from a page dropped \u{b7} {why}"));
                self.status.hold();
            }
        }
        self.window.request_redraw();
    }

    /// The bar's panel on the chrome's grid.
    pub(in crate::ui) fn reply_dialog(&self, now: Instant) -> Option<CloseDialog> {
        self.reply_panel(&self.reply_bar.view(now)?)
    }

    fn reply_panel(&self, view: &ReplyView) -> Option<CloseDialog> {
        let size = self.canvas.grid_size().ok()?;
        Some(close_dialog(
            ratatui::layout::Rect::new(0, 0, size.width, size.height),
            CloseDialogText::Reply { to: &view.to, line: &view.line, keys: &view.keys },
        ))
    }

    /// How the bar is laid out now: a close dialog covers it, or its panel is cut off by a small window, or all of it
    /// shows, at this rectangle.
    fn reply_sight(&self) -> Sight {
        let panel = self.reply_bar.layout_view().and_then(|view| self.reply_panel(&view));
        sight_of(self.close_dialog_open(), panel.as_ref())
    }

    /// Tell the bar how it is laid out. Called with each frame's overlays and before each key is routed.
    pub(in crate::ui) fn observe_reply_bar(&mut self) {
        let sight = self.reply_sight();
        self.reply_bar.observe(sight);
    }

    /// A frame was presented: the bar, if that frame laid it out whole, has now been shown.
    pub(in crate::ui) fn reply_bar_presented(&mut self, now: Instant) {
        self.reply_bar.presented(now);
    }

    /// Give the bar a key; whether it took it. An armed Enter sends the reply, Esc drops it.
    pub(in crate::ui) fn reply_bar_key(&mut self, key: BarKeyKind, now: Instant) -> bool {
        let said = match self.reply_bar.key(key, now) {
            BarKey::Pass => return false,
            BarKey::Taken => None,
            BarKey::Dropped => {
                tracing::info!("page reply: dropped");
                Some("reply dropped".to_string())
            }
            BarKey::Send(reply) => {
                let (host, workspace_id, handle, line) = reply.into_parts();
                tracing::info!(%handle, "page reply: confirmed");
                let sent = self.send_to(&host, OutgoingReq::PageReply { workspace_id, handle: handle.clone(), line });
                Some(match sent {
                    Ok(()) => format!("sending reply to {handle}\u{2026}"),
                    Err(_) => format!("reply to {handle} not sent \u{b7} no connection to its daemon"),
                })
            }
        };
        if let Some(text) = said {
            self.status.set(text);
            self.status.hold();
        }
        self.window.request_redraw();
        true
    }
}

/// The bar's native State fixture: a `State` exists only inside a native window, so it runs in the window_minimized
/// target's fixtures child (`ui/app/native_children.rs`).
#[cfg(all(test, feature = "test-window-progress"))]
#[path = "reply_native_fixture_tests.rs"]
mod native_fixture;
