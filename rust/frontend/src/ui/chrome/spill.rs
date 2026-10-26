//! The nav spill overlay: which nav rows repaint over the preview, and cell-width truncation.

use crate::ui::nav::Mode;

/// One nav row the spill overlay repaints in full over the preview's
/// left edge (nav-spill, `[nav] spill_ms`). Collected in the draw
/// closure for VISIBLE rows whose text overflows the nav column;
/// consumed by the overlay backing-quad + overlay-text draws at the END
/// of the render pass. Cell coordinates — converted to px at paint time
/// with the same `chrome_origin + cell * cell_w/h` math as the
/// underlying chrome row, so the nav-resident prefix realigns
/// pixel-identically and the row reads as one continuous run.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::ui) struct NavSpillSeg {
    /// Absolute cell column of the row's first glyph (the nav content
    /// rect's left edge).
    pub(in crate::ui) x: u16,
    /// Absolute cell row on the grid.
    pub(in crate::ui) row: u16,
    /// The full row text (already truncated to the overlay's reach cap).
    pub(in crate::ui) text: String,
    /// Display width of `text` in cells (chars ≈ cells; see collection
    /// site comment on the double-width approximation).
    pub(in crate::ui) width_cells: u16,
    /// Resolved fg + modifiers mirroring the underlying row's style so
    /// the overlay is colour-identical to what it covers.
    pub(in crate::ui) color: Option<(u8, u8, u8)>,
    pub(in crate::ui) bold: bool,
    pub(in crate::ui) dim: bool,
}

/// Nav-spill overflow decision: should a `char_w`-cell row in a
/// `nav_w`-cell nav column spill, and if so how many chars survive the
/// reach cap? `Some(n)` = overlay the first `n` chars; `None` = the row
/// fits (or there is no reach to spill into). Pure so the policy is
/// testable outside the draw closure.
pub(in crate::ui) fn nav_spill_take(cell_w: usize, nav_w: usize, max_cells: usize) -> Option<usize> {
    if cell_w <= nav_w || max_cells == 0 {
        return None;
    }
    Some(cell_w.min(max_cells))
}

/// Truncate `text` to at most `cells` terminal cells (unicode-width — a CJK
/// glyph counts 2), returning the kept prefix and its exact cell width. A
/// double-width glyph that would straddle the boundary is dropped, so the
/// result NEVER exceeds `cells` (codex review: chars().count() let wide
/// glyphs overrun the backing strip).
pub(in crate::ui) fn truncate_to_cells(text: &str, cells: usize) -> (String, usize) {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut w = 0usize;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > cells {
            break;
        }
        out.push(ch);
        w += cw;
    }
    (out, w)
}

#[cfg(test)]
#[path = "spill_tests.rs"]
mod tests;

/// What the spill trigger compares between frames: (mode, workspace, tree cursor, picker cursor, tree generation).
pub(in crate::ui) type SpillCursor = (Mode, Option<String>, usize, Option<usize>, u64);

/// The nav spill's state between frames: the deadline a user's cursor move arms, the segments the last draw
/// collected, and the cursor the trigger last saw.
#[derive(Default)]
pub(in crate::ui) struct NavSpill {
    /// Transient nav spill (`[nav] spill_ms`): while the user is actively
    /// moving the nav cursor, nav rows whose text overflows the column
    /// float their FULL text over the preview pane's left edge (overlay
    /// text layer + backing strip — pane geometry never moves), vanishing
    /// once this deadline passes with no further moves. `None` = spill
    /// not active. Expiry repaint rides the `about_to_wait` idle tick,
    /// same as the sticky toast.
    until: Option<std::time::Instant>,
    /// The spill rows collected by the LAST draw-closure run — the
    /// `media_paint_targets` pattern: the closure writes cell-space
    /// segments here, the render-pass tail reads them for the backing
    /// quads and the overlay text prepare. Empty whenever the spill is
    /// inactive or nothing overflows.
    segments: Vec<NavSpillSeg>,
    /// Last cursor the spill trigger saw — a change between frames is what (re)arms the timer.
    /// Frame-side detection so every cursor mover (keys, wheel, mode and
    /// workspace switches) triggers uniformly without touching each input
    /// path. `None` until the first frame, which deliberately does not
    /// arm (boot isn't a user move).
    cursor: Option<SpillCursor>,
}

impl NavSpill {
    /// The frame's cursor. Arm ONLY on a user cursor move: same mode+workspace, same tree
    /// CONTENT (generation), different cursor. Everything else that
    /// perturbs the tuple — boot, the async initial tree load, a
    /// cursor restore, a refresh splicing rows around the cursor, a
    /// mode/workspace switch swapping the tree — updates the baseline
    /// without arming (codex review: first-frame-only suppression
    /// made startup spill timing-dependent). `spill_ms` 0 never arms.
    pub(in crate::ui) fn observe(&mut self, now: SpillCursor, spill_ms: u64) {
        if self.cursor.as_ref() == Some(&now) {
            return;
        }
        let user_move = match (self.cursor.as_ref(), &now) {
            (Some((pm, pw, ps, pp, pg)), (m, w, s, p, g)) => pm == m && pw == w && pg == g && (ps != s || pp != p),
            (None, _) => false,
        };
        self.cursor = Some(now);
        if user_move && spill_ms > 0 {
            self.until = Some(std::time::Instant::now() + std::time::Duration::from_millis(spill_ms));
        }
    }

    pub(in crate::ui) fn active(&self) -> bool {
        self.until.map(|u| std::time::Instant::now() < u).unwrap_or(false)
    }

    /// The idle tick: true once, when the deadline has passed (the nav column springs back on the repaint).
    pub(in crate::ui) fn expire(&mut self) -> bool {
        match self.until {
            Some(until) if std::time::Instant::now() >= until => {
                self.until = None;
                true
            }
            _ => false,
        }
    }

    pub(in crate::ui) fn segments(&self) -> &[NavSpillSeg] { &self.segments }
    pub(in crate::ui) fn publish(&mut self, segments: Vec<NavSpillSeg>) { self.segments = segments; }
    pub(in crate::ui) fn clear_segments(&mut self) { self.segments.clear(); }
}
