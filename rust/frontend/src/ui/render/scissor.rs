//! The pixel scissor of a rectangle: one rule for every pass stage that clips a draw to a pane.

use super::quad::ScreenRect;

/// `rect` as a wgpu scissor on a `surface`: each edge rounded to the nearest pixel, as rasterization takes a quad's pixels by
/// their centers (so a clipped draw lies on the pixels the pane's own fills lie on), then cut to the surface. `None` when no
/// pixel is left.
pub(in crate::ui) fn scissor_px(rect: ScreenRect, surface: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let edge = |v: f32, max: u32| (v.round().max(0.0) as u32).min(max);
    let (left, top) = (edge(rect.x, surface.0), edge(rect.y, surface.1));
    let (right, bottom) = (edge(rect.x + rect.w, surface.0), edge(rect.y + rect.h, surface.1));
    (right > left && bottom > top).then_some((left, top, right - left, bottom - top))
}

/// Clips the pass to `rect` (an empty scissor when none of it is on the surface, so nothing draws).
pub(in crate::ui) fn set_scissor(rpass: &mut wgpu::RenderPass<'_>, rect: ScreenRect, surface: (u32, u32)) {
    let (x, y, w, h) = scissor_px(rect, surface).unwrap_or((0, 0, 0, 0));
    rpass.set_scissor_rect(x, y, w, h);
}

#[cfg(test)]
#[path = "scissor_tests.rs"]
mod tests;
