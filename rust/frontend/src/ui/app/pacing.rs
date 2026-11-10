//! The frame pacer: the frame-rate cap's deferred frame and clock, and the first frame's focus.

use super::FRAME_BUDGET;

/// At most one frame per `FRAME_BUDGET`: a redraw inside the budget is deferred (dirty) and drawn at the boundary,
/// so a burst collapses into one frame. The harness (`--capture`) draws every redraw.
pub(in crate::ui) struct FramePacer {
    /// Frame-rate cap state. `request_redraw` from event handlers and the
    /// transport task queue `RedrawRequested`; if we'd draw twice within
    /// `FRAME_BUDGET`, the second one sets `dirty` and `about_to_wait`
    /// reschedules at the next frame boundary so a burst (paste, PTY echo
    /// storm, LLM token stream) collapses into one frame.
    dirty: bool,
    /// An instant a frame is wanted at, with none drawn before it: the chat's next short-paced ask. Set afresh by each
    /// frame (`wake_at`), so a stale one is never kept.
    wake: Option<std::time::Instant>,
    /// When the last frame was drawn; `None` before the first.
    last_frame_at: Option<std::time::Instant>,
    /// One-shot: focus + raise the window on the first rendered frame.
    /// A focus request made at window-creation time (before the window
    /// is shown / before the first paint) is widely ignored by window
    /// managers — Windows' foreground-lock and macOS both restrict it.
    /// Deferring to the first frame is the portable way to land focused
    /// on launch / after an ADR-0017 self-relaunch. When the OS blocks
    /// focus-stealing outright we fall back to `request_user_attention`
    /// (taskbar flash / dock bounce / urgent hint). Cleared after use.
    focus_on_first_frame: bool,
}

/// When a deferred frame is due.
pub(in crate::ui) enum FrameDue {
    /// Draw now (the deferred mark is cleared).
    Now,
    /// Wake at this instant and draw then.
    At(std::time::Instant),
}

impl FramePacer {
    pub(in crate::ui) fn new() -> Self { Self { dirty: false, wake: None, last_frame_at: None, focus_on_first_frame: true } }

    /// A frame is wanted (an animation still moving, a debounce still waiting).
    pub(in crate::ui) fn mark_dirty(&mut self) { self.dirty = true; }
    /// A frame is owed: a deferred one, or one at `wake`.
    pub(in crate::ui) fn is_dirty(&self) -> bool { self.dirty || self.wake.is_some() }
    /// A frame is wanted at `at` and none before it (`None`: none wanted).
    pub(in crate::ui) fn wake_at(&mut self, at: Option<std::time::Instant>) { self.wake = at; }
    pub(in crate::ui) fn clear_dirty(&mut self) { self.dirty = false; }

    /// A redraw request: whether it falls inside the budget, in which case it is deferred (marked dirty); else the
    /// mark clears and the frame draws.
    pub(in crate::ui) fn defer(&mut self, harness: bool) -> bool {
        let throttled = !harness && self.last_frame_at.map(|t| t.elapsed() < FRAME_BUDGET).unwrap_or(false);
        self.dirty = throttled;
        throttled
    }

    /// The frame was drawn at `at`.
    pub(in crate::ui) fn drawn(&mut self, at: std::time::Instant) { self.last_frame_at = Some(at); }

    /// The deferred frame (the pacer is dirty): now when the budget has passed since the last frame, else at its
    /// boundary.
    pub(in crate::ui) fn due(&mut self) -> FrameDue {
        // Only a wake is owed: the loop sleeps to it, and the frame is drawn when it is reached.
        if !self.dirty {
            match self.wake.take() {
                Some(at) if at > std::time::Instant::now() => {
                    self.wake = Some(at);
                    return FrameDue::At(at);
                }
                _ => return FrameDue::Now,
            }
        }
        match self.last_frame_at {
            Some(t) => {
                let elapsed = t.elapsed();
                if elapsed >= FRAME_BUDGET {
                    self.dirty = false;
                    FrameDue::Now
                } else {
                    FrameDue::At(std::time::Instant::now() + (FRAME_BUDGET - elapsed))
                }
            }
            None => {
                self.dirty = false;
                FrameDue::Now
            }
        }
    }

    /// The first drawn frame takes the window's focus, once.
    pub(in crate::ui) fn take_first_frame_focus(&mut self) -> bool { std::mem::take(&mut self.focus_on_first_frame) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A frame wanted at an instant is slept to, not drawn before it: the pacer is not dirty for the frame budget, `due`
    /// answers `At` that instant, and once it has passed the frame is due now.
    #[test]
    fn a_wake_is_slept_to_and_draws_nothing_before_it() {
        let mut p = FramePacer::new();
        let at = Instant::now() + Duration::from_secs(60);
        p.wake_at(Some(at));
        assert!(!p.dirty, "no frame is deferred");
        assert!(p.is_dirty(), "a frame is owed, so the loop asks `due`");
        assert!(matches!(p.due(), FrameDue::At(t) if t == at));
        assert!(matches!(p.due(), FrameDue::At(t) if t == at), "asking again changes nothing");
        p.wake_at(Some(Instant::now()));
        assert!(matches!(p.due(), FrameDue::Now), "reached: the frame is due now");
        assert!(!p.is_dirty(), "and the wake is spent");
        p.wake_at(Some(at));
        p.wake_at(None);
        assert!(!p.is_dirty(), "a wake no longer wanted is dropped");
    }
}
