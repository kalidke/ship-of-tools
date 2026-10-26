//! The session strip's motion: its eased horizontal scroll and the brand wheels' spin.

use std::time::Instant;

use super::band::{STRIP_TAU, WHEEL_FLICK_VEL, WHEEL_MAX_VEL, WHEEL_MIN_VEL, WHEEL_TAU};

/// Both animations run on the frame clock and report whether they still move, so the frame loop stays alive only
/// until they settle.
#[derive(Default)]
pub(in crate::ui) struct StripMotion {
    /// Session-strip horizontal scroll, in strip-local pixels (the
    /// strip-local x that maps to screen-center). `None` = uninitialized
    /// → snap to the active session's center on first paint (no slide-in).
    /// Eases toward the active session's center each frame.
    scroll_px: Option<f32>,
    /// Timestamp of the last strip-animation frame, for frame-rate-
    /// independent easing. `None` when settled (so the next switch starts
    /// a fresh ease rather than seeing a stale dt).
    anim_last: Option<Instant>,
    /// Brand-wheel spin gimmick (see `WHEEL_*`): current rotation of the
    /// bottom-strip bow wheels (radians), the live angular velocity a
    /// workspace cycle flicks it with, and the last spin-frame timestamp for
    /// frame-rate-independent decay (`None` when at rest).
    wheel_angle: f32,
    wheel_vel: f32,
    wheel_anim_last: Option<Instant>,
}

impl StripMotion {
    /// The strip's scroll, or `target` before the first paint.
    pub(in crate::ui) fn scroll_or(&self, target: f32) -> f32 { self.scroll_px.unwrap_or(target) }
    pub(in crate::ui) fn wheel_angle(&self) -> f32 { self.wheel_angle }

    /// A workspace cycle flicks the wheels in the direction of travel (forward = CW).
    pub(in crate::ui) fn flick(&mut self, direction: i32) {
        self.wheel_vel = (self.wheel_vel + direction as f32 * WHEEL_FLICK_VEL).clamp(-WHEEL_MAX_VEL, WHEEL_MAX_VEL);
    }

    /// One frame at `now`: ease the scroll toward `target` and spin the wheels down. Returns whether either still
    /// moves (the frame loop must stay alive).
    pub(in crate::ui) fn step(&mut self, scroll: f32, target: f32, now: Instant) -> bool {
        // Frame-rate-independent ease-out.
        let mut moving = false;
        let dt = self
            .anim_last
            .map(|t| (now - t).as_secs_f32().min(0.1))
            .unwrap_or(0.0);
        let k = if dt > 0.0 {
            1.0 - (-dt / STRIP_TAU).exp()
        } else {
            0.0
        };
        let next = scroll + (target - scroll) * k;
        if (target - next).abs() < 0.5 {
            self.scroll_px = Some(target);
            self.anim_last = None;
        } else {
            self.scroll_px = Some(next);
            self.anim_last = Some(now);
            moving = true;
        }
        // Spin the brand wheels down on the same frame clock: advance the
        // angle by the current velocity, decay the velocity (frame-rate
        // independent), and keep the loop alive until it settles. The angle
        // is left wherever it stops — a wheel rests fine at any rotation.
        if self.wheel_vel.abs() > WHEEL_MIN_VEL {
            let wdt = self
                .wheel_anim_last
                .map(|t| (now - t).as_secs_f32().min(0.1))
                .unwrap_or(0.0);
            self.wheel_angle += self.wheel_vel * wdt;
            self.wheel_vel *= (-wdt / WHEEL_TAU).exp();
            if self.wheel_vel.abs() <= WHEEL_MIN_VEL {
                self.wheel_vel = 0.0;
                self.wheel_anim_last = None;
            } else {
                self.wheel_anim_last = Some(now);
                moving = true;
            }
        }
        moving
    }
}
