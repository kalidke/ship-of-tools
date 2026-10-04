//! Colours and the status-change flash: brightness scaling, flash ramp, expiry.

use super::*;

impl State {
    /// Status-change flash factor for `(host, slug)` at `now` (1.0 right at
    /// the transition, fading to 0.0 over `FLASH_SECS`). `0.0` when the
    /// slug has no live flash. Pure read so both the (immutable-borrow)
    /// nav-row build and the strip caller can use it.
    pub(in crate::ui) fn flash_factor_for(&self, host: &str, slug: &str, now: std::time::Instant) -> f32 {
        let key: WsKey = (host.to_string(), slug.to_string());
        self.flash_starts
            .get(&key)
            .map(|t| flash_factor(now.duration_since(*t).as_secs_f32()))
            .unwrap_or(0.0)
    }

    /// Drop `flash_starts` entries whose fade has fully elapsed so the map
    /// stays small and `about_to_wait` stops scheduling fast repaints once no
    /// flash is live. Returns true while any flash is still animating.
    pub(in crate::ui) fn prune_expired_flashes(&mut self, now: std::time::Instant) -> bool {
        let window = std::time::Duration::from_secs_f32(FLASH_SECS);
        self.flash_starts
            .retain(|_, t| now.duration_since(*t) < window);
        !self.flash_starts.is_empty()
    }
}

/// Scale an RGB colour's brightness by `f` (saturating). `f > 1.0`
/// brightens (toward white-ish, channel-clamped); `f < 1.0` dims. Used by
/// the selected-session contrast levers so a single multiplier expresses
/// both "pop the selection brighter" and "fade the non-selected".
pub(in crate::ui) fn scale_rgb(rgb: (u8, u8, u8), f: f32) -> (u8, u8, u8) {
    let s = |c: u8| -> u8 { ((c as f32 * f).round()).clamp(0.0, 255.0) as u8 };
    (s(rgb.0), s(rgb.1), s(rgb.2))
}

/// Brightness multiplier applied to a *non-selected* session name under the
/// "dim" contrast lever, so the selected name pops by contrast. Distinct
/// from (stronger than) the existing wilt/idle ~0.65 dim.
pub(in crate::ui) const CONTRAST_DIM_FACTOR: f32 = 0.55;

/// Alpha of the nav-spill overlay's backing strip (0–255). Near-opaque:
/// high enough that spilled row text reads crisply over any preview
/// content, low enough that an image preview barely ghosts through —
/// signalling "floating above the preview", not "the preview has a hole".
/// Tune by eye on a real image preview.
pub(in crate::ui) const NAV_SPILL_BACK_ALPHA: u8 = 242;
/// Brightness multiplier applied to the *selected* coloured session name
/// under the "bright" lever, so the active row's tone reads clearly brighter
/// than non-selected coloured rows (which keep their full tone).
pub(in crate::ui) const CONTRAST_BRIGHT_FACTOR: f32 = 1.35;

/// Resolve a coloured (tone-bearing) state-nav session name to its final
/// `(rgb, bold, dim)` under the active contrast lever — shared by the
/// Sessions-mode nav rows and the bottom strip so the two stay pixel-aligned.
/// `wilted` is the stale-"working" flag (forces a dim). `is_active` is the
/// selected/active row. `contrast_dim` is the `--contrast-mode dim` lever.
///
/// bright lever: the active coloured row keeps its tone hue but is scaled up
/// (`CONTRAST_BRIGHT_FACTOR`) so it out-reads non-active coloured rows; bold.
/// dim lever: non-active coloured rows are scaled down
/// (`CONTRAST_DIM_FACTOR`); the active row keeps its full tone + bold. Either
/// way the tone hue + the bold-composes-with-colour behaviour is preserved.
///
/// `flash_f` (0..1, the `flash_factor`) is composed *last*: the resolved
/// colour is lerped toward white by `flash_f` so a just-changed name blinks
/// bright then fades back to its contrast-adjusted tone.
pub(in crate::ui) fn contrast_tone_rgb(
    tone: AgentTone,
    wilted: bool,
    is_active: bool,
    contrast_dim: bool,
    flash_f: f32,
) -> (Option<(u8, u8, u8)>, bool, bool) {
    let base = tone.rgb();
    let rgb = match (is_active, contrast_dim) {
        // bright lever, active coloured row → push the tone brighter.
        (true, false) => base.map(|c| scale_rgb(c, CONTRAST_BRIGHT_FACTOR)),
        // dim lever, non-active coloured row → fade it back.
        (false, true) => base.map(|c| scale_rgb(c, CONTRAST_DIM_FACTOR)),
        _ => base,
    };
    // Flash composes on top of the contrast result.
    let rgb = if flash_f > 0.0 {
        rgb.map(|c| lerp_to_white(c, flash_f))
    } else {
        rgb
    };
    (rgb, is_active, wilted)
}

/// Agent work-state tone for the state-nav Sessions render (ADR 0023). Maps
/// the registry `agent_state` to a row colour identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) enum AgentTone {
    Working,
    Idle,
    Waiting,
    Blocked,
    Done,
}

impl AgentTone {
    /// Base row colour, pinned to the Julia brand palette so the session
    /// tones read as one coherent logo+session palette (maintainer directive via
    /// sot-docs) — all 4 Julia brand colours + gray, no yellow:
    ///   working = Julia green  #389826 (live),
    ///   waiting = Julia purple #9558B2 (delegated to a long job; idle-of-own-
    ///             work but not free),
    ///   blocked = Julia red    #CB3C33 (needs attention — the loud one),
    ///   done    = Julia blue   #4063D8 (finished),
    ///   idle    = gray (quiet; no Julia equiv).
    /// `rgb()` routes these through `chrome::ratatui_color_to_rgb`, which passes
    /// `Color::Rgb` straight through, so the nav rows and bottom strip pin to
    /// the exact hex.
    fn color(self) -> Color {
        match self {
            AgentTone::Working => Color::Rgb(56, 152, 38), // Julia green  #389826
            AgentTone::Idle => Color::Gray,
            AgentTone::Waiting => Color::Rgb(149, 88, 178), // Julia purple #9558B2
            AgentTone::Blocked => Color::Rgb(203, 60, 51),  // Julia red    #CB3C33
            AgentTone::Done => Color::Rgb(64, 99, 216),     // Julia blue   #4063D8
        }
    }

    /// Same tone as raw RGB, routed through the chrome's ratatui→RGB table so
    /// the bottom session strip (which draws raw `text::Line` colours, not
    /// ratatui styles) matches the Sessions-mode nav rows exactly.
    pub(in crate::ui) fn rgb(self) -> Option<(u8, u8, u8)> {
        crate::chrome::ratatui_color_to_rgb(Some(self.color()))
    }
}

/// Minutes after which a still-"working" agent that hasn't re-stamped its
/// status is treated as stale and wilted (dimmed). Only "working" ages —
/// idle/waiting/done are resting states, and a long "blocked" stays loud on
/// purpose.
pub(in crate::ui) const AGENT_STALE_MINUTES: i64 = 10;

/// Duration of the status-change flash (ADR 0023): when a session's
/// work-state changes, its name brightens toward white and fades to its
/// resting tone over this window. Short enough to read as a blink, long
/// enough to catch the eye. Shared by the nav rows + the bottom strip.
const FLASH_SECS: f32 = 0.6;

/// Flash brightness factor for a name whose state changed `elapsed` ago:
/// 1.0 right at the transition, ramping linearly to 0.0 at `FLASH_SECS`,
/// clamped outside the window. Callers lerp the name colour toward white by
/// this factor. Pulled out so the ramp stays unit-testable.
fn flash_factor(elapsed_secs: f32) -> f32 {
    (1.0 - elapsed_secs / FLASH_SECS).clamp(0.0, 1.0)
}

/// Lerp an RGB colour toward white by `t` (0.0 = unchanged, 1.0 = full
/// white). Used to brighten a flashing session name; `t` is the
/// `flash_factor`. Saturating cast keeps it within `u8`.
pub(in crate::ui) fn lerp_to_white(rgb: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let lerp = |c: u8| -> u8 { (c as f32 + (255.0 - c as f32) * t).round() as u8 };
    (lerp(rgb.0), lerp(rgb.1), lerp(rgb.2))
}

/// Apply a flash to a plain (non-tone) strip name with a concrete base
/// colour: lerp toward white by `flash` when it's live, else leave the base
/// untouched. Keeps the strip's plain-branch flash composition in one place.
pub(in crate::ui) fn flash_plain(flash: f32, base: (u8, u8, u8)) -> (u8, u8, u8) {
    if flash > 0.0 {
        lerp_to_white(base, flash)
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_bright_pops_active_coloured_row() {
        // Bright lever (contrast_dim=false): the active coloured row is
        // scaled up past its base tone and bold; a non-active coloured row
        // keeps its full base tone, not bold.
        let base = AgentTone::Working.rgb().unwrap();
        let (act_rgb, act_bold, act_dim) =
            contrast_tone_rgb(AgentTone::Working, false, true, false, 0.0);
        assert!(act_bold && !act_dim);
        let act = act_rgb.unwrap();
        assert!(
            act.0 >= base.0 && act.1 >= base.1 && act.2 >= base.2 && act != base,
            "active bright row must be >= base tone and brighter overall"
        );
        // Non-active keeps base, no bold.
        let (na_rgb, na_bold, _) = contrast_tone_rgb(AgentTone::Working, false, false, false, 0.0);
        assert_eq!(na_rgb, Some(base));
        assert!(!na_bold);
    }

    #[test]
    fn contrast_dim_fades_non_active_coloured_row() {
        // Dim lever (contrast_dim=true): the active coloured row keeps its
        // base tone + bold; a non-active coloured row is scaled down.
        let base = AgentTone::Blocked.rgb().unwrap();
        let (act_rgb, act_bold, _) = contrast_tone_rgb(AgentTone::Blocked, false, true, true, 0.0);
        assert_eq!(act_rgb, Some(base));
        assert!(act_bold);
        let (na_rgb, na_bold, _) = contrast_tone_rgb(AgentTone::Blocked, false, false, true, 0.0);
        let na = na_rgb.unwrap();
        assert!(
            na.0 < base.0 && na.1 <= base.1 && na.2 <= base.2,
            "non-active dim row must be darker than base"
        );
        assert!(!na_bold);
    }

    #[test]
    fn flash_factor_ramps_down_over_window() {
        // Full brightness at the instant of the change.
        assert!((flash_factor(0.0) - 1.0).abs() < 1e-6);
        // Half-faded at half the window.
        assert!((flash_factor(FLASH_SECS / 2.0) - 0.5).abs() < 1e-6);
        // Zero at (and past) the window end — clamped, never negative.
        assert_eq!(flash_factor(FLASH_SECS), 0.0);
        assert_eq!(flash_factor(FLASH_SECS * 2.0), 0.0);
        assert_eq!(flash_factor(-1.0), 1.0);
    }
}
