//! The Terminal drawer's backend: the local shell's pty, the size it was last given, and the folder it starts in.

use super::pty::{LocalTerminal, ResolvedShell};
use super::vt::scroll_ring;

/// The drawer's local shell (G2/G3). Spawned the first time the drawer is shown, at 80x24, in the repo root; sized
/// to the drawer rect once it is known and on every later change.
pub(in crate::ui) struct TerminalDrawer {
    /// Local PTY terminal hosting the OS shell (G2). A separate field
    /// from the ratatui `terminal` so the draw closure's `self.terminal`
    /// borrow and this terminal's `screen()` borrow are disjoint.
    term: Option<LocalTerminal>,
    /// Last `(cols, rows)` the local terminal's PTY was sized to. `None`
    /// until the drawer rect is first observed; drives resize-on-change
    /// (mirrors the agent pane size).
    size: Option<(u16, u16)>,
    /// Local repo root (`$SOT_REPO_DIR`, set by the supervisor). Used as
    /// the Terminal drawer's working directory, so the plain shell it spawns
    /// starts in the project root. `None` when launched outside the
    /// supervisor (then the shell inherits the frontend's cwd). ADR 0017.
    repo_dir: Option<std::path::PathBuf>,
}

/// One frame's pump of the shell: what the status line needs when the pane looks wrong.
pub(in crate::ui) struct PumpReport {
    pub(in crate::ui) processed: bool,
    pub(in crate::ui) dead: bool,
    pub(in crate::ui) has_content: bool,
    pub(in crate::ui) cols: u16,
    pub(in crate::ui) rows: u16,
}

impl TerminalDrawer {
    pub(in crate::ui) fn new(repo_dir: Option<std::path::PathBuf>) -> Self { Self { term: None, size: None, repo_dir } }

    pub(in crate::ui) fn is_spawned(&self) -> bool { self.term.is_some() }
    pub(in crate::ui) fn screen(&self) -> Option<&vt100::Screen> { self.term.as_ref().map(|t| t.screen()) }
    pub(in crate::ui) fn mouse_tracking_on(&self) -> bool {
        self.term.as_ref().map(|t| t.mouse_tracking_on()).unwrap_or(false)
    }

    /// Spawn the shell at 80x24 in the repo root; `wake` runs after each chunk of output.
    pub(in crate::ui) fn spawn(
        &mut self, shell: &ResolvedShell, wake: Box<dyn Fn() + Send + 'static>,
    ) -> anyhow::Result<()> {
        // cwd = repo root, so the plain shell starts in the
        // project directory. ADR 0017.
        let t = LocalTerminal::spawn(shell, 80, 24, self.repo_dir.as_deref(), wake)?;
        self.term = Some(t);
        Ok(())
    }

    /// Drain pending output into the parser before the draw borrows the screen; `None` with no shell.
    pub(in crate::ui) fn pump(&mut self) -> Option<PumpReport> {
        let t = self.term.as_mut()?;
        let processed = t.pump();
        let dead = t.is_dead();
        let screen = t.screen();
        let (rows, cols) = screen.size();
        let mut has_content = false;
        'scan: for r in 0..rows {
            for c in 0..cols {
                if let Some(cell) = screen.cell(r, c) {
                    if !cell.contents().is_empty() {
                        has_content = true;
                        break 'scan;
                    }
                }
            }
        }
        Some(PumpReport { processed, dead, has_content, cols, rows })
    }

    /// The drawer rect in cells: the pty is resized when it differs from the size last given (at least 2x2).
    pub(in crate::ui) fn fit(&mut self, cols: u16, rows: u16) {
        if cols >= 2 && rows >= 2 {
            let changed = self.size.map(|prev| prev != (cols, rows)).unwrap_or(true);
            if changed {
                if let Some(t) = self.term.as_mut() {
                    t.resize(cols, rows);
                }
                self.size = Some((cols, rows));
            }
        }
    }

    /// Walk the vt100 scrollback ring by `delta` rows (positive is older).
    pub(in crate::ui) fn scroll(&mut self, delta: i32) {
        if let Some(t) = self.term.as_mut() {
            scroll_ring(t.screen_mut(), delta);
        }
    }

    /// Input that should leave the view at the live tail (typing, a paste).
    pub(in crate::ui) fn type_input(&mut self, bytes: &[u8]) {
        if let Some(t) = self.term.as_mut() {
            t.send_input(bytes);
            t.screen_mut().set_scrollback(0);
        }
    }

    /// Input forwarded as is, `times` times (a wheel the shell's app tracks).
    pub(in crate::ui) fn forward_input(&mut self, bytes: &[u8], times: u32) {
        if let Some(t) = self.term.as_mut() {
            for _ in 0..times {
                t.send_input(bytes);
            }
        }
    }
}
