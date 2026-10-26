//! The reply bar: a reply a page offers, held for the person's word. Only its Enter makes a `ReplySend`.

use std::time::{Duration, Instant};

use ratatui::layout::Rect;

use crate::net::dial::HostKey;

/// How long the bar must have been on screen, whole, before Enter sends: no key meant for something else lands on it.
pub(in crate::ui) const ARM: Duration = Duration::from_secs(1);

/// A reply a page offered, as the window resolved it: the daemon it came from, the row's id and listed name, the
/// session's handle and the exact line that will be typed.
pub(in crate::ui) struct ReplyOffer {
    pub(in crate::ui) host: HostKey,
    pub(in crate::ui) workspace_id: String,
    pub(in crate::ui) handle: String,
    pub(in crate::ui) name: String,
    pub(in crate::ui) line: String,
}

/// Why an offer opened no bar.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) enum OfferRefused {
    /// The person has not answered the offer already open.
    Busy,
    /// The line is not exactly what the daemon's grammar makes of a page's text, so the bar could not show what
    /// would be typed.
    Line(String),
}

/// The confirmed reply. Only `ReplyBar::key` makes one, from an armed Enter, so nothing else can send a reply.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) struct ReplySend {
    host: HostKey,
    workspace_id: String,
    handle: String,
    line: String,
}

impl ReplySend {
    /// The daemon to send to (the one the offer came from), the row's id, the handle and the line.
    pub(in crate::ui) fn into_parts(self) -> (HostKey, String, String, String) {
        (self.host, self.workspace_id, self.handle, self.line)
    }
}

/// A key as the bar reads it: logical, and never through a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum BarKeyKind {
    /// A plain Enter; `repeat` is a held key's autorepeat.
    Enter { repeat: bool },
    Esc,
    Other,
}

/// What the bar did with a key.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) enum BarKey {
    /// A dialog covers the bar, or none is open: another layer owns the key.
    Pass,
    /// The bar took the key, to do nothing.
    Taken,
    Send(ReplySend),
    Dropped,
}

/// What the panel says: who the reply is for, the exact line, and what the keys do now.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) struct ReplyView {
    pub(in crate::ui) to: String,
    pub(in crate::ui) line: String,
    pub(in crate::ui) keys: String,
}

/// What the person can see of the bar, which is what decides who gets a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum Sight {
    /// A dialog is drawn over it: the dialog takes every key and the bar none.
    Covered,
    /// The window is too short or too narrow to draw every row of the line: Esc drops it, but Enter cannot send what
    /// is not shown.
    Cut,
    /// Laid out in full, at this panel rectangle. Another rectangle (a resize) is another sight.
    Whole(Rect),
}

struct Open {
    offer: ReplyOffer,
    sight: Sight,
    /// When a frame laying the bar out `Whole` was last presented; `None` until one is, and again as soon as the
    /// sight changes. Layout alone is no evidence: a frame can fail after it.
    shown_since: Option<Instant>,
}

/// The bar holds at most one offer, and is told what is drawn on top of it (`offer`, `observe`) and when a frame
/// that showed it was presented (`presented`).
#[derive(Default)]
pub(in crate::ui) struct ReplyBar {
    open: Option<Open>,
}

impl ReplyBar {
    /// Hold `offer` for an answer. Nothing has been shown yet.
    pub(in crate::ui) fn offer(&mut self, offer: ReplyOffer, sight: Sight) -> Result<(), OfferRefused> {
        if self.open.is_some() {
            return Err(OfferRefused::Busy);
        }
        sot_protocol::page_reply::check_line(&offer.line).map_err(|r| OfferRefused::Line(r.to_string()))?;
        self.open = Some(Open { offer, sight, shown_since: None });
        Ok(())
    }

    /// Tell the bar how it is laid out now. Any change of sight (a dialog over it, a resize, a cut) drops the
    /// evidence that it was shown, so the wait starts over from the next presented frame: no Enter meant for a
    /// dialog, or typed while the bar was hidden, finds it armed.
    pub(in crate::ui) fn observe(&mut self, sight: Sight) {
        if let Some(open) = &mut self.open {
            if open.sight != sight {
                open.shown_since = None;
            }
            open.sight = sight;
        }
    }

    /// A frame has been presented, laid out as the last `observe` said. If that was the whole bar, the wait begins.
    pub(in crate::ui) fn presented(&mut self, now: Instant) {
        if let Some(open) = &mut self.open {
            if matches!(open.sight, Sight::Whole(_)) && open.shown_since.is_none() {
                open.shown_since = Some(now);
            }
        }
    }

    /// Whether a dialog is drawn over an open bar, so the dialog owns every key and nothing under it gets one.
    pub(in crate::ui) fn covered(&self) -> bool {
        self.open.as_ref().is_some_and(|open| open.sight == Sight::Covered)
    }

    /// Enter sends once the bar has been shown whole for `ARM` and is not a repeat; Esc drops; every other key is
    /// taken. While a dialog covers the bar it takes none.
    pub(in crate::ui) fn key(&mut self, key: BarKeyKind, now: Instant) -> BarKey {
        let Some(open) = self.open.as_ref().filter(|open| open.sight != Sight::Covered) else {
            return BarKey::Pass;
        };
        let armed = open.shown_since.is_some_and(|since| now.saturating_duration_since(since) >= ARM);
        match key {
            BarKeyKind::Esc => {
                self.open = None;
                BarKey::Dropped
            }
            BarKeyKind::Enter { repeat: false } if armed => {
                let Open { offer, .. } = self.open.take().expect("an open bar was just read");
                BarKey::Send(ReplySend {
                    host: offer.host,
                    workspace_id: offer.workspace_id,
                    handle: offer.handle,
                    line: offer.line,
                })
            }
            BarKeyKind::Enter { .. } | BarKeyKind::Other => BarKey::Taken,
        }
    }

    /// When the wait before Enter ends, while it is still running: the moment the panel changes its words.
    pub(in crate::ui) fn arm_deadline(&self, now: Instant) -> Option<Instant> {
        let since = self.open.as_ref()?.shown_since?;
        (now.saturating_duration_since(since) < ARM).then(|| since + ARM)
    }

    /// The panel's words. The offer's text is the daemon's and the row's: draw it through `cell_lines`.
    pub(in crate::ui) fn view(&self, now: Instant) -> Option<ReplyView> {
        let Open { shown_since, .. } = self.open.as_ref()?;
        let wait = shown_since.map_or(ARM, |since| ARM.saturating_sub(now.saturating_duration_since(since)));
        self.view_waiting(wait)
    }

    /// The panel as it is laid out, for deciding whether it shows whole: always the words of the wait, the longer of
    /// the two, so the panel's rectangle does not change when Enter arms and no change of words counts as a new sight.
    pub(in crate::ui) fn layout_view(&self) -> Option<ReplyView> {
        self.view_waiting(ARM)
    }

    fn view_waiting(&self, wait: Duration) -> Option<ReplyView> {
        let offer = &self.open.as_ref()?.offer;
        let keys = if wait.is_zero() {
            "Enter sends \u{b7} Esc drops".to_string()
        } else {
            format!("Enter in {} s \u{b7} Esc drops", wait.as_secs_f64().ceil() as u64)
        };
        Some(ReplyView {
            to: format!("to {} \u{b7} {} on {}", offer.handle, offer.name, offer.host),
            line: offer.line.clone(),
            keys,
        })
    }
}

#[cfg(test)]
#[path = "reply_bar_tests.rs"]
mod tests;
