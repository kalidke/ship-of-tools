//! The preview's own message in a box over the preview pane. `PreviewNotice` owns the text and the instant it was shown;
//! it knows no `State`. A newer message replaces the shown one at once, the box is held and fades as the help peek is
//! (`hold_then_fade`), and a row switch clears it. `boxed` is the pure layout: the text as cell-safe rows wrapped to the
//! pane, at the pane's foot so the top of a picture stays clear.

use crate::ui::chrome::{cell_lines, wrap_status};
use crate::ui::input::layout_help::{hold_then_fade, PEEK_HOLD};
use ratatui::layout::Rect;
use std::time::Instant;

/// The most text rows a notice takes; a longer text ends its last row with an ellipsis (the status line has it whole).
pub(in crate::ui) const MAX_ROWS: usize = 6;
/// Narrower than this, the pane's text rows are too few cells to read and no box is drawn.
const MIN_TEXT_CELLS: usize = 8;

#[derive(Default)]
pub(in crate::ui) struct PreviewNotice {
    shown: Option<(String, Instant)>,
}

/// The box, in the chrome grid's cells: its rectangle and the rows the text takes, two cells in from the left and right
/// as the help peek's text is, one blank row above and below.
pub(in crate::ui) struct NoticeBox {
    pub rect: Rect,
    pub lines: Vec<String>,
    pub opacity: f32,
}

impl PreviewNotice {
    pub(in crate::ui) fn show(&mut self, text: String, now: Instant) { self.shown = Some((text, now)); }
    pub(in crate::ui) fn clear(&mut self) { self.shown = None; }
    pub(in crate::ui) fn text(&self) -> Option<&str> { self.shown.as_ref().map(|(t, _)| t.as_str()) }

    /// Opacity at `now`: 1 while held, then to 0 over the fade; 0 when nothing is shown.
    pub(in crate::ui) fn opacity(&self, now: Instant) -> f32 {
        self.shown.as_ref().map_or(0.0, |(_, at)| hold_then_fade(*at, now))
    }

    /// Drops a notice whose fade is over; true once, when it did, so the frame that erases it is drawn.
    pub(in crate::ui) fn expire(&mut self, now: Instant) -> bool {
        if self.shown.is_some() && self.opacity(now) <= 0.0 { self.shown = None; true } else { false }
    }

    pub(in crate::ui) fn fading(&self, now: Instant) -> bool {
        self.shown.as_ref().is_some_and(|(_, at)| now.saturating_duration_since(*at) >= PEEK_HOLD)
    }

    /// When the hold ends and the fade begins.
    pub(in crate::ui) fn deadline(&self) -> Option<Instant> { self.shown.as_ref().map(|(_, at)| *at + PEEK_HOLD) }

    /// The box inside `pane` (the preview's content rectangle, in cells) at `now`, flush with the pane's foot. None when
    /// nothing is shown, the fade is over or the pane is too small to hold two text rows' padding and a few cells.
    pub(in crate::ui) fn boxed(&self, pane: Rect, now: Instant) -> Option<NoticeBox> {
        let (text, _) = self.shown.as_ref()?;
        let opacity = self.opacity(now);
        let width = (pane.width as usize).checked_sub(4).filter(|w| *w >= MIN_TEXT_CELLS)?;
        let rows = (pane.height as usize).checked_sub(2).map(|room| room.min(MAX_ROWS)).filter(|r| *r > 0)?;
        let mut lines: Vec<String> = cell_lines(text).iter().flat_map(|row| wrap_status(row, width, width)).collect();
        if lines.is_empty() || opacity <= 0.0 {
            return None;
        }
        if lines.len() > rows {
            lines.truncate(rows);
            if let Some(last) = lines.last_mut() { ellipsize(last, width) }
        }
        let height = lines.len() as u16 + 2;
        Some(NoticeBox { rect: Rect { x: pane.x, y: pane.y + pane.height - height, width: pane.width, height }, lines, opacity })
    }
}

/// `row` ended with an ellipsis within `width` cells.
fn ellipsize(row: &mut String, width: usize) {
    use unicode_width::UnicodeWidthStr;
    while !row.is_empty() && row.width() + 1 > width { row.pop(); }
    row.push('…');
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
