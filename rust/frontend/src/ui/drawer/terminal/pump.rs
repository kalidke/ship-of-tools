//! The Terminal drawer's frame work: lazy spawn and pump of the shell before the draw, and the pty resize after it.

use crate::ui::*;

impl State {
    pub(in crate::ui) fn pump_drawer_terminals(&mut self) {
        // Local terminal drawer (G2/G3): lazily spawn the OS shell the
        // first time the Terminal drawer is shown, then drain any pending
        // output into its parser before we borrow its screen for the draw.
        // All mutation happens here, before the `self.terminal.draw`
        // borrow and the immutable `local_term` screen borrow below.
        // ADR 0041 step 6 U3: `drawer.attach_only` picks the Terminal
        // drawer's BACKEND, once, the first time the drawer opens this
        // session. Off (the default) or off-Windows: `use_attach_only`
        // is always `false` and every line below behaves exactly as it
        // did before this unit — "When off, NOTHING the FE does today
        // changes."
        #[cfg(windows)]
        let use_attach_only = drawer_uses_attach(
            self.settings.attach_only,
            self.attach_term.is_some(),
            self.local_term.is_some(),
            &self.leases.granted_state_roots(),
            self.own_state_root.as_deref(),
        );
        #[cfg(not(windows))]
        let use_attach_only = false;

        // ADR 0041 step 6 U3 ruling (a), Codex review round finding 2:
        // spawn (gated on the drawer being open) is separate from pump
        // (which runs on EVERY redraw regardless of drawer visibility).
        #[cfg(windows)]
        if self.drawer == DrawerContent::Terminal && use_attach_only && self.attach_term.is_none() {
            self.spawn_attach_term();
        }
        #[cfg(windows)]
        if self.attach_term.is_some() {
            self.pump_attach_term();
        }

        if self.drawer == DrawerContent::Terminal && !use_attach_only {
            if self.local_term.is_none() {
                #[cfg(windows)]
                if self.settings.attach_only {
                    self.status =
                        "attach-only terminal needs this computer's backend to hold this window; opened a plain terminal"
                            .to_string();
                }
                let shell = crate::ui::drawer::terminal::pty::resolve_shell(self.settings.terminal_shell.as_deref());
                let waker = self.window.clone();
                // cwd = repo root, so the plain shell starts in the
                // project directory. ADR 0017.
                let cwd = self.repo_dir.clone();
                match crate::ui::drawer::terminal::pty::LocalTerminal::spawn(
                    &shell,
                    80,
                    24,
                    cwd.as_deref(),
                    Box::new(move || waker.request_redraw()),
                ) {
                    Ok(t) => {
                        tracing::info!(program = %shell.program, "local terminal spawned");
                        self.local_term = Some(t);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to spawn local terminal");
                        self.status = format!("terminal spawn failed: {e}");
                        // Fall back to closing the drawer so the user isn't
                        // staring at an empty pane with no explanation.
                        self.drawer = DrawerContent::Closed;
                    }
                }
            }
            if let Some(t) = self.local_term.as_mut() {
                let processed = t.pump();
                // Diagnostic surfaced on the status line so a blank pane is
                // debuggable without RUST_LOG: parser size, dead flag, and
                // whether the screen currently holds any non-blank cell.
                // Only overwrite the status line when the pane actually looks
                // wrong (dead / no content) — otherwise it fires every frame
                // the drawer is open and clobbers real status messages
                // (connection line, pin/unpin, ADR-0019 `notify`).
                let dead = t.is_dead();
                let screen = t.screen();
                let (srows, scols) = screen.size();
                let mut has_content = false;
                'scan: for r in 0..srows {
                    for c in 0..scols {
                        if let Some(cell) = screen.cell(r, c) {
                            if !cell.contents().is_empty() {
                                has_content = true;
                                break 'scan;
                            }
                        }
                    }
                }
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
        if tcols >= 2 && trows >= 2 {
            let changed = self
                .term_size
                .map(|prev| prev != (tcols, trows))
                .unwrap_or(true);
            if changed {
                if let Some(t) = self.local_term.as_mut() {
                    t.resize(tcols, trows);
                }
                #[cfg(windows)]
                if let Some(t) = self.attach_term.as_mut() {
                    t.resize(tcols, trows);
                }
                self.term_size = Some((tcols, trows));
            }
        }
    }
}
