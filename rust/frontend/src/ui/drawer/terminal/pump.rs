//! The Terminal drawer's frame work: lazy spawn and pump of the shell before the draw, and the pty resize after it;
//! and the scrollback ring's step.

use crate::ui::*;
use super::drawer::PumpReport;

/// `scroll_ring` for the Terminal drawer's local terminal; three call sites share it.
pub(in crate::ui) fn scroll_drawer_ring(state: &mut State, delta: i32) {
    state.term_drawer.scroll(delta);
}

impl State {
    pub(in crate::ui) fn pump_drawer_terminals(&mut self) {
        // Local terminal drawer (G2/G3): lazily spawn the OS shell the
        // first time the Terminal drawer is shown, then drain any pending
        // output into its parser before we borrow its screen for the draw.
        // All mutation happens here, before the `self.terminal.draw`
        // borrow and the immutable terminal screen borrow below.
        if self.layout_help.drawer() == DrawerContent::Terminal {
            if !self.term_drawer.is_spawned() {
                let shell = crate::ui::drawer::terminal::pty::resolve_shell(self.settings.terminal_shell.as_deref());
                let wake = self.wake.clone();
                match self.term_drawer.spawn(&shell, Box::new(move || wake.wake())) {
                    Ok(()) => {
                        tracing::info!(program = %shell.program, "local terminal spawned");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to spawn local terminal");
                        self.status = format!("terminal spawn failed: {e}");
                        // Fall back to closing the drawer so the user isn't
                        // staring at an empty pane with no explanation.
                        self.layout_help.terminal_spawn_failed();
                    }
                }
            }
            // Diagnostic surfaced on the status line so a blank pane is
            // debuggable without RUST_LOG: parser size, dead flag, and
            // whether the screen currently holds any non-blank cell.
            // Only overwrite the status line when the pane actually looks
            // wrong (dead / no content) — otherwise it fires every frame
            // the drawer is open and clobbers real status messages
            // (connection line, pin/unpin, ADR-0019 `notify`).
            if let Some(PumpReport { processed, dead, has_content, cols: scols, rows: srows }) = self.term_drawer.pump() {
                if dead || !has_content {
                    self.status = format!(
                        "term: {scols}x{srows} content={has_content} dead={dead} pumped={processed}"
                    );
                }
            }
        }
    }

    pub(in crate::ui) fn sync_terminal_drawer_size(&mut self, term_size_observed: (u16, u16)) {
        // Resize the local terminal's PTY to the drawer rect once it's
        // known (G3). Spawned at a default 80x24; this snaps it to the
        // real drawer size on the first frame it's visible, and on any
        // later drawer geometry change.
        let (tcols, trows) = term_size_observed;
        self.term_drawer.fit(tcols, trows);
    }
}
