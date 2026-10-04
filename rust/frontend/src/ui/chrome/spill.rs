//! The nav spill overlay: which nav rows repaint over the preview, and cell-width truncation.

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
mod tests {
    use super::*;

    #[test]
    fn nav_spill_take_fits_and_overflows() {
        // Fits exactly — no spill.
        assert_eq!(nav_spill_take(20, 20, 40), None);
        // One over — spill the whole row.
        assert_eq!(nav_spill_take(21, 20, 40), Some(21));
        // Overflow beyond the reach — clamp to the cap.
        assert_eq!(nav_spill_take(100, 20, 40), Some(40));
        // No reach (preview absent / zero-width) — never spill.
        assert_eq!(nav_spill_take(100, 20, 0), None);
        // Degenerate nav width still respects the cap.
        assert_eq!(nav_spill_take(5, 0, 3), Some(3));
    }

    #[test]
    fn truncate_to_cells_respects_wide_glyphs() {
        // ASCII: 1 cell each.
        assert_eq!(truncate_to_cells("abcdef", 4), ("abcd".to_string(), 4));
        // CJK: 2 cells each — "日本語" is 6 cells; a 5-cell budget keeps two
        // glyphs (4 cells) rather than straddling the boundary.
        assert_eq!(truncate_to_cells("日本語", 5), ("日本".to_string(), 4));
        assert_eq!(truncate_to_cells("日本語", 6), ("日本語".to_string(), 6));
        // Budget larger than the text returns it whole at its true width.
        assert_eq!(truncate_to_cells("ab", 10), ("ab".to_string(), 2));
    }
}
