//! The status line's text, the hold every word keeps on it, and the note a disconnect line carries.

use super::status::NOTIFY_STICKY;
use std::time::Instant;

/// One-line status string for the chrome. A word (`set`) replaces the text at once and holds it for `NOTIFY_STICKY`
/// from its own write; a state line (`show_state`: one re-said by a timer, a frame, a redial or a re-list) is written
/// only while no hold is live and holds nothing, so it never replaces a word before the word's hold ends.
pub(in crate::ui) struct StatusLine {
    text: String,
    /// While `Some(t)` and `now < t`, the line holds the last word written, and a state line leaves it. Cleared, and
    /// the connection line rebuilt, once elapsed (`State::expire_status_hold`, on the idle tick); a clear ends it.
    sticky_until: Option<Instant>,
    /// The held text a disconnect line carries (a reply's outcome, a notify): kept on its own, so each further
    /// disconnect line carries it and not the previous disconnect line. Any other write drops it.
    note: Option<String>,
}

impl StatusLine {
    pub(in crate::ui) fn new(text: impl Into<String>) -> Self { Self { text: text.into(), sticky_until: None, note: None } }

    pub(in crate::ui) fn text(&self) -> &str { &self.text }

    /// A word: replaces the text at once and holds it for `NOTIFY_STICKY`.
    pub(in crate::ui) fn set(&mut self, text: impl Into<String>) { self.set_at(text, Instant::now()); }

    /// `set` at `now`.
    pub(in crate::ui) fn set_at(&mut self, text: impl Into<String>, now: Instant) {
        self.text = text.into();
        self.note = None;
        self.hold(now);
    }

    /// A state line: written only while no word is held, and holds nothing.
    pub(in crate::ui) fn show_state(&mut self, text: impl Into<String>) { self.show_state_at(text, Instant::now()); }

    /// `show_state` at `now`.
    pub(in crate::ui) fn show_state_at(&mut self, text: impl Into<String>, now: Instant) {
        if !self.held(now) {
            self.text = text.into();
            self.note = None;
        }
    }

    /// Empties the line and ends its hold.
    pub(in crate::ui) fn clear(&mut self) {
        self.text.clear();
        self.note = None;
        self.sticky_until = None;
    }

    /// The text a disconnect line must carry: the note an earlier one carried, else the text held now (never a
    /// disconnect line itself), while the hold lasts.
    pub(in crate::ui) fn carried(&mut self, now: Instant) -> Option<String> {
        if !self.held(now) {
            return None;
        }
        self.note.clone().or_else(|| Some(self.text.clone()).filter(|t| !t.starts_with("disconnected")))
    }

    /// Write `text` carrying `note`, and hold it.
    pub(in crate::ui) fn set_carrying(&mut self, text: impl Into<String>, note: String, now: Instant) {
        self.text = text.into();
        self.note = Some(note);
        self.hold(now);
    }

    /// Hold the text just written for `NOTIFY_STICKY` from `now`.
    fn hold(&mut self, now: Instant) { self.sticky_until = Some(now + NOTIFY_STICKY); }

    /// Whether a word still holds the line at `now`; an elapsed hold is dropped.
    fn held(&mut self, now: Instant) -> bool {
        if let Some(until) = self.sticky_until {
            if now < until {
                return true;
            }
            self.sticky_until = None;
        }
        false
    }

    /// The idle tick: true once, when a hold has elapsed at `now` (the caller rebuilds the line).
    pub(in crate::ui) fn expire_hold(&mut self, now: Instant) -> bool {
        match self.sticky_until {
            Some(until) if now >= until => {
                self.sticky_until = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod tests;
