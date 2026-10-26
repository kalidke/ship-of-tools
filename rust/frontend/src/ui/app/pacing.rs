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
    pub(in crate::ui) fn new() -> Self { Self { dirty: false, last_frame_at: None, focus_on_first_frame: true } }

    /// A frame is wanted (an animation still moving, a debounce still waiting).
    pub(in crate::ui) fn mark_dirty(&mut self) { self.dirty = true; }
    pub(in crate::ui) fn is_dirty(&self) -> bool { self.dirty }
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
