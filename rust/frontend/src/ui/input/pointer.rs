//! The pointer's state between events: its last position and the wheel's sub-row residue.

/// winit reports the cursor only on motion and the wheel in fractions, so both are kept between events.
#[derive(Default)]
pub(in crate::ui) struct Pointer {
    /// Latest cursor position in physical pixels — winit only
    /// delivers `CursorMoved` on motion, so a click immediately after
    /// focus would otherwise have no position to anchor to.
    at_px: (f32, f32),
    /// Fractional wheel-row accumulator. Precision touchpads and smooth
    /// wheels emit small sub-row deltas; truncating each event to i32
    /// drops everything until the user flicks hard. Accumulating here
    /// and only applying whole rows lets gentle scroll work the way the
    /// user expects.
    residue_y: f32,
}

impl Pointer {
    pub(in crate::ui) fn at_px(&self) -> (f32, f32) { self.at_px }
    pub(in crate::ui) fn moved_to(&mut self, px: (f32, f32)) { self.at_px = px; }
    pub(in crate::ui) fn residue(&self) -> f32 { self.residue_y }
    pub(in crate::ui) fn store_residue(&mut self, residue: f32) { self.residue_y = residue; }
    /// A target that consumed the wheel's fractional rows (the agent pane) starts the next gesture clean.
    pub(in crate::ui) fn clear_residue(&mut self) { self.residue_y = 0.0; }
}
