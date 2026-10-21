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
