//! The status line's text and the sticky window a pushed notify holds it for.

use super::status::NOTIFY_STICKY;

/// One-line status string for the chrome. Every write replaces the text; a pushed notify also holds it for
/// `NOTIFY_STICKY`, during which `rebuild_connection_status` does not replace it.
pub(in crate::ui) struct StatusLine {
    text: String,
    /// While `Some(t)` and `now < t`, the status line is holding a pushed
    /// notify (op::FE_COMMAND `notify`) and `rebuild_connection_status` won't
    /// clobber it — so a toast survives a workspace switch for a few seconds
    /// instead of being overwritten instantly (which otherwise made a pushed
    /// notify un-seeable while the user roamed workspaces). Cleared + the
    /// status rebuilt once elapsed (in `about_to_wait`, on the idle tick).
    sticky_until: Option<std::time::Instant>,
}

impl StatusLine {
    pub(in crate::ui) fn new(text: impl Into<String>) -> Self { Self { text: text.into(), sticky_until: None } }

    pub(in crate::ui) fn text(&self) -> &str { &self.text }
    pub(in crate::ui) fn set(&mut self, text: impl Into<String>) { self.text = text.into(); }
    pub(in crate::ui) fn clear(&mut self) { self.text.clear(); }

    /// Hold the text just written for `NOTIFY_STICKY`.
    pub(in crate::ui) fn hold(&mut self) { self.sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY); }

    /// Whether a held notify still holds the line; an elapsed hold is dropped.
    pub(in crate::ui) fn held(&mut self) -> bool {
        if let Some(until) = self.sticky_until {
            if std::time::Instant::now() < until {
                return true;
            }
            self.sticky_until = None;
        }
        false
    }

    /// The idle tick: true once, when a hold has elapsed (the caller rebuilds the line).
    pub(in crate::ui) fn expire_hold(&mut self) -> bool {
        match self.sticky_until {
            Some(until) if std::time::Instant::now() >= until => {
                self.sticky_until = None;
                true
            }
            _ => false,
        }
    }
}
