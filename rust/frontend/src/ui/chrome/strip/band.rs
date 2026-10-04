//! The band's rows, the scroll target and animation constants, and the strip's text lines.

use super::*;

/// Easing time-constant (seconds) for the strip slide — ~50 ms → an
/// exponential ease-out that settles in ~150 ms, frame-rate independent.
pub(in crate::ui) const STRIP_TAU: f32 = 0.05;

/// Wheel-spin gimmick: cycling workspaces (`Shift+←/→` → `cycle_workspace`)
/// flicks the brand wheel at every ship's bow in the bottom strip — forward
/// spins them clockwise, backward counter-clockwise — and they spin down. Each
/// cycle adds `WHEEL_FLICK_VEL` rad/s (signed by direction, clamped to
/// `WHEEL_MAX_VEL` so a burst of presses doesn't blur), decaying with time
/// constant `WHEEL_TAU`; below `WHEEL_MIN_VEL` the spin settles and the angle
/// just rests wherever it stopped (a wheel looks fine at any rotation).
pub(in crate::ui) const WHEEL_FLICK_VEL: f32 = 13.0;
pub(in crate::ui) const WHEEL_MAX_VEL: f32 = 42.0;
pub(in crate::ui) const WHEEL_TAU: f32 = 0.45;
pub(in crate::ui) const WHEEL_MIN_VEL: f32 = 0.05;

/// Rows `cell_grid_for` keeps OUT of the chrome grid, to make room BELOW it for
/// the band. The band hangs off the grid's bottom edge (`strip_row_tops`) while
/// the chrome fills the grid, so the rows it lives in have to come off the grid
/// exactly once, here.
///
/// `need` is the band's own height in px — air, the hull's offset, the drop to
/// the waterline, the line itself and the sea — and NOT a count of text rows.
/// That is the change: the panes keep whole rows, the band does not. At scale 1
/// it is `0 + 14 + 11 + 3 + 2 = 30`, against `1 * cell_h + oy = 30`, so ONE row
/// clears it exactly and the pane row the second one used to cost comes back.
/// Every term is a fraction of `BASE_CELL_H`, so that holds at every scale.
///
/// Erring high is safe — one spare row is a slightly shorter pane — while erring
/// low runs the waterline off the window's bottom edge, so `k` is the ceiling.
/// Sitting exactly ON the budget is not free: a sum of f32 ratios lands a few
/// millionths of a px above a budget it exactly meets, and `BAND_FIT_EPS_ROWS`
/// is what keeps that hair from taking a whole row.
/// `strip_band_never_touches_the_grids_last_row` pins the condition, k's
/// minimality and that constancy.
pub(in crate::ui) fn strip_reserved_rows(cell_h: f32, oy: f32) -> u16 {
    let need = cell_h
        * (STRIP_TOP_AIR_ROWS
            + HULL_ROW_OFFSET_ROWS
            + HULL_DROP_ROWS
            + HULL_THICKNESS_ROWS
            + STRIP_SEA_ROWS);
    (((need - oy) / cell_h.max(1.0) - BAND_FIT_EPS_ROWS)
        .ceil()
        .max(0.0)) as u16
}

/// Rounding slack for the band's fit, in rows, and the reason a band that
/// exactly meets its budget is allowed to say so.
///
/// `need` is a sum of ratios of `BASE_CELL_H` scaled by `cell_h`, so it carries
/// f32 error. When the band's five lengths together spend the whole of its
/// budget, their sum of ratios rounds UP and `need` lands a few
/// millionths of a pixel above a budget it exactly meets. Without this slack
/// that hair takes a THIRD reserved row and every pane loses a line of content
/// — a rounding artefact charged to the user as lost screen, and one that does
/// not show at the default window, only at small scale.
///
/// A thousandth of a row is some three orders of magnitude above the error and
/// three below one pixel, so it absorbs the artefact while a real one-pixel
/// overrun still takes the row it needs. It is a tolerance on a comparison,
/// not a fudge to the geometry: no length moves.
const BAND_FIT_EPS_ROWS: f32 = 1e-3;

/// Strip-local center-x (pixels) of the active item — the value
/// `strip_scroll_px` eases toward so the active session sits at screen
/// center. Items lay out left→right, each `len*cell_w` wide with
/// `STRIP_GAP_CELLS*cell_w` between them; `divider_offsets[active]` (0.0
/// when absent — `&[]` from a caller with no `Bow`s to place)
/// shifts the target over by however much strip space precedes it
/// (`strip_divider_offsets`).
pub(in crate::ui) fn session_strip_target(
    labels: &[String],
    active: usize,
    cell_w: f32,
    divider_offsets: &[f32],
) -> f32 {
    let gap = STRIP_GAP_CELLS * cell_w;
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| l.chars().count() as f32 * cell_w)
        .collect();
    let positions = strip_cursor_positions(&widths, |_| gap);
    match (positions.get(active), widths.get(active)) {
        (Some(&pos), Some(&w)) => {
            pos + w / 2.0 + divider_offsets.get(active).copied().unwrap_or(0.0)
        }
        _ => 0.0,
    }
}

/// Build the pixel-positioned `Line`s for the bottom session strip. The
/// active item is centered at `win_w/2` (via `scroll_px`) and bold; items
/// fully off the window are culled. `baseline_y` is the glyph-top y in
/// physical pixels.
///
/// `tones[i]` (parallel to `labels`, `None` past its end) colours each name by
/// its agent's work-state — working = green, waiting = yellow, blocked = red,
/// done = blue — so a
/// session that's running or waiting on you stands out *even when it isn't the
/// active one* (that's the at-a-glance value). Idle / no-agent names keep the
/// original styling (active = bright, others = dim), matching "idle = current
/// colour". Colours come from `AgentTone::rgb`, so the strip and the
/// Sessions-mode rows are pixel-identical. A wilted (stale) "working" dims.
///
/// `contrast_dim` is the `--contrast-mode` lever: under "bright" (false) the
/// active name pops by going brighter + bold; under "dim" (true) the
/// *non*-active names are faded so the active one pops by contrast. Applied
/// to every name (coloured or not), routed through `contrast_tone_rgb` for
/// the coloured ones so it matches the nav rows exactly.
///
/// `flashes[i]` (parallel to `labels`, `0.0` past its end) is the
/// status-change flash factor for that name — when `> 0` its colour is
/// lerped toward white (composed after the contrast lever), so a name whose
/// work-state just changed blinks bright then fades back.
///
/// `pendings[i]` (parallel to `labels`, `false` past its end) is the badge
/// floor (ADR 0025 §1) flag: a workspace with a pending nav.preview result
/// waiting. Its `●` sigil is already in `labels[i]` (`strip_label`, so the
/// hull measured it too); here the flag only sets bright white + bold
/// (overriding the tone/contrast colour) so it reads as "a result is
/// waiting here", distinct from the work-state colours. Non-disruptive — it
/// only changes how the name renders, never the view.
///
/// `divider_offsets[i]` (parallel to `labels`, `0.0` past its end — `&[]`
/// from a caller with nothing interleaved to place) shifts session i's
/// cursor position over by however much strip space the non-session items
/// before it occupy (`strip_divider_offsets`); the offscreen cull is applied
/// AFTER the shift so a session pushed off-window by a preceding divider is
/// culled correctly, not by its pre-shift position.
pub(in crate::ui) fn session_strip_lines(
    labels: &[String],
    active: usize,
    scroll_px: f32,
    win_w: f32,
    cell_w: f32,
    baseline_y: f32,
    tones: &[Option<(AgentTone, bool)>],
    contrast_dim: bool,
    flashes: &[f32],
    pendings: &[bool],
    divider_offsets: &[f32],
) -> Vec<crate::ui::render::text::Line> {
    let gap = STRIP_GAP_CELLS * cell_w;
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| l.chars().count() as f32 * cell_w)
        .collect();
    // Same dividers-oblivious cursor walk `session_strip_target` uses;
    // `divider_offsets` (see the doc above) folds the `Bow`s back in.
    let positions = strip_cursor_positions(&widths, |_| gap);
    let mut out = Vec::new();
    for (i, lab) in labels.iter().enumerate() {
        let w = widths[i];
        let cursor = positions[i] + divider_offsets.get(i).copied().unwrap_or(0.0);
        let left = strip_screen_left(cursor, scroll_px, win_w);
        if !strip_visible(left, w, win_w) {
            continue; // fully off-screen
        }
        let pending = pendings.get(i).copied().unwrap_or(false);
        let is_active = i == active;
        let flash = flashes.get(i).copied().unwrap_or(0.0);
        // Working/waiting/blocked/done override the colour so they're visible
        // regardless of which session is active. Idle (and no agent state)
        // keep the pre-state-nav styling so "idle = current colour" holds.
        // The contrast lever composes on top via `contrast_tone_rgb` for the
        // coloured branch; for the plain branches the "dim" lever bakes an
        // explicitly-dimmed colour (text.rs's fixed 0.65 DIM can't be made
        // stronger through the `dim` flag alone). The flash is applied last.
        let (color, dim) = match tones.get(i).copied().flatten() {
            Some((
                tone @ (AgentTone::Working
                | AgentTone::Waiting
                | AgentTone::Blocked
                | AgentTone::Done),
                wilted,
            )) => {
                let (rgb, _bold, d) =
                    contrast_tone_rgb(tone, wilted, is_active, contrast_dim, flash);
                (rgb, d)
            }
            _ if is_active => (Some(flash_plain(flash, (250, 250, 215))), false),
            // Non-active idle / no-agent name. "dim" lever fades it harder
            // than the default DIM by baking the colour; "bright" keeps the
            // pre-state-nav dim styling.
            _ if contrast_dim => (
                Some(flash_plain(
                    flash,
                    scale_rgb((204, 204, 204), CONTRAST_DIM_FACTOR),
                )),
                false,
            ),
            // Plain non-active under "bright": keep the default DIM unless a
            // flash is live, in which case resolve the default fg + lerp it.
            _ if flash > 0.0 => (Some(lerp_to_white((204, 204, 204), flash)), false),
            _ => (None, true),
        };
        // Badge floor (ADR 0025 §1): a pending name overrides whatever tone /
        // contrast colour it would otherwise get with bright white + bold
        // (matching the nav-row badge), clearing the dim so the `●`-prefixed
        // name reads as "result waiting here" — distinct from the work-state
        // colours without adding another hue.
        let (color, dim) = if pending {
            (Some((255, 255, 255)), false)
        } else {
            (color, dim)
        };
        out.push(crate::ui::render::text::Line {
            text: lab.clone(),
            x: left,
            // Lift the active name a bit so it pops by VERTICAL position, not
            // just colour (it was hard to tell the active session from the
            // bottom). Smaller y = higher; top-left origin.
            y: if is_active {
                baseline_y - STRIP_ACTIVE_LIFT_CELLS * cell_w
            } else {
                baseline_y
            },
            color,
            bold: is_active || pending,
            italic: false,
            dim,
        });
    }
    out
}

#[cfg(test)]
#[path = "band_tests.rs"]
mod tests;
