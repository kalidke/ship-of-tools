//! The capture harness's run: the PNG to write and when, a selfie asked for, and the capture one-shots
//! (`--capture-preview`, `--capture-cycle`).

use std::path::PathBuf;

use super::capture::CAPTURE_FRAME;

/// What a frame must capture: the harness's one shot (then exit) or a selfie (keep running), which the harness shot
/// beats when both fall on one frame.
pub(in crate::ui) struct FrameShot {
    pub(in crate::ui) target: Option<PathBuf>,
    pub(in crate::ui) harness: bool,
    pub(in crate::ui) selfie: Option<PathBuf>,
}

pub(in crate::ui) struct CaptureRun {
    /// `--capture <path>`: render to PNG and exit. Set means we keep
    /// requesting redraws until `frames == CAPTURE_FRAME` so the
    /// transport task has time to push events.
    path: Option<PathBuf>,
    /// Ctrl+Shift+S selfie: a pending whole-window capture target. Set by the
    /// keybind, consumed by the render loop on the next frame — unlike
    /// `path`, the FE does NOT exit after (it's a live screenshot).
    selfie: Option<PathBuf>,
    /// `--capture-preview <relpath>`: project-relative path to fire
    /// `preview.get` for as soon as the first Files-mode `tree.root` lands.
    /// One-shot — consumed on first dispatch.
    preview: Option<String>,
    /// True when `--capture-preview` was supplied. Persists past the
    /// one-shot `preview` consumption so the readback frame
    /// delay knows to wait for the extra round-trip + MathJax.
    preview_armed: bool,
    /// Explicit `--capture-delay-ms` override (0 = auto). When non-zero,
    /// the readback frame is `delay_ms * 60 / 1000` regardless of
    /// `preview_armed`.
    delay_ms: u32,
    /// `--capture-cycle <N>`: simulate N presses of Shift+ArrowRight (or
    /// `|N|` of Shift+ArrowLeft when negative) on the first `workspace.list`
    /// reply. One-shot — consumed on first application so a re-fetch
    /// later doesn't re-cycle. Zero leaves the active workspace alone.
    cycle: i32,
    /// Frames drawn so far; the harness shot is taken at its target frame.
    frames: u32,
}

impl CaptureRun {
    pub(in crate::ui) fn new(
        path: Option<PathBuf>, preview: Option<String>, delay_ms: u32, cycle: i32,
    ) -> Self {
        let preview_armed = preview.is_some();
        Self { path, selfie: None, preview, preview_armed, delay_ms, cycle, frames: 0 }
    }

    /// Whether this is a `--capture` run (it paces itself and exits after its shot).
    pub(in crate::ui) fn harness(&self) -> bool { self.path.is_some() }

    /// Frames drawn so far, for the native fixtures that wait on them.
    #[cfg(all(test, feature = "test-window-progress"))]
    pub(in crate::ui) fn frames(&self) -> u32 { self.frames }

    pub(in crate::ui) fn count_frame(&mut self) { self.frames += 1; }

    /// Ctrl+Shift+S: the next frame is written to `path`, and the window keeps running.
    pub(in crate::ui) fn request_selfie(&mut self, path: PathBuf) { self.selfie = Some(path); }

    /// This frame's shot. --capture-preview adds a second async round-trip (preview.get for
    /// a specific file) on top of the connect-time root preview. Math
    /// also has to wait on the MathJax sidecar per `$$…$$` block. Give
    /// it more frames so the readback is taken after the math SVGs have
    /// landed and been laid out. A pending selfie is taken either way.
    pub(in crate::ui) fn shot(&mut self) -> FrameShot {
        let target_frame = if self.delay_ms > 0 {
            // Explicit override from --capture-delay-ms. Redraw loop is
            // 60 Hz, so ms * 60 / 1000.
            (self.delay_ms * 60 / 1000).max(1)
        } else if self.preview_armed {
            CAPTURE_FRAME * 4
        } else {
            CAPTURE_FRAME
        };
        let harness = self.path.is_some() && self.frames == target_frame;
        let selfie = self.selfie.take();
        let target = if harness { self.path.clone() } else { selfie.clone() };
        FrameShot { target, harness, selfie }
    }

    /// Whether `--capture-preview` has yet to fire (the README default stands down until it has).
    pub(in crate::ui) fn preview_pending(&self) -> bool { self.preview.is_some() }

    /// `--capture-preview`'s file, once.
    pub(in crate::ui) fn take_preview(&mut self) -> Option<String> { self.preview.take() }

    /// `--capture-preview` was given: the cursor's own preview stands down for the whole run.
    pub(in crate::ui) fn preview_armed(&self) -> bool { self.preview_armed }

    /// `--capture-cycle`'s steps, once (0 after).
    pub(in crate::ui) fn take_cycle(&mut self) -> i32 { std::mem::take(&mut self.cycle) }
}
