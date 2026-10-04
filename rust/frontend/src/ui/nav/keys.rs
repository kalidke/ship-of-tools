//! What a key does in the navigation tree: its own keys and the per-row action match, in order.

use super::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;
use crate::ui::session::keys::{picker_key, session_destroy_key, session_enter_key};
use crate::ui::nav::files::keys::{nav_file_key, nav_prompt_key};

pub(in crate::ui) fn nav_tree_key(state: &mut State, event_loop: &ActiveEventLoop, key: KeyPress<'_>, label: String, was_destroy_pending: Option<WsKey>) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    picker_key(state, key)?;
    nav_prompt_key(state, key)?;
    // NavTree focus: Ctrl+Q is the *only* way to exit
    // the interactive window. Plain q and Esc no longer
    // quit — Esc is used constantly in the LLM pane
    // (vim, readline interrupt, claude prompt cancel)
    // and the user routinely double-taps it; making
    // it lethal turned every reflex into a quit risk.
    // Ctrl+Q is scoped to NavTree only so it doesn't
    // collide with terminal flow-control (XOFF) in
    // the BL pty. Capture mode sets `should_exit` on
    // its own and never sees user input.
    if !event.repeat
        && action == Some(Action::Quit)
    {
        state.request_quit(event_loop, ExitReason::QuitKey);
        return Break(());
    }
    // Ctrl+C: copy the cursored row's file path to the
    // OS clipboard. Only fires for `files:`-prefixed
    // node ids (Files mode + Modules-mode rows that
    // reuse the synthesized files: id for previews);
    // sessions / picker / workspace rows pass through.
    // Ctrl+C is reserved-as-interrupt in the LLM pty
    // but in NavTree there's no pty, so the universal
    // copy convention reads cleanly here.
    if !event.repeat
        && action == Some(Action::CopyPath)
        && state.copy_navtree_path()
    {
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    // Ctrl+N: open the new-file-or-folder prompt. Files
    // mode only, and only when the cursor sits on a
    // `files:` row (begin_create_file no-ops otherwise
    // and falls through to normal nav). A plain name
    // reuses `file.write` with empty content; a name
    // ending in `/` fires `dir.create` instead
    // (confirm_create_file picks which).
    if !event.repeat
        && action == Some(Action::NewFile)
        && matches!(state.mode, Mode::Files)
        && state.begin_create_file()
    {
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    // Ctrl+D: open the delete-confirm prompt. Files mode
    // only, and only when the cursor sits on a deletable
    // `files:` file row (begin_delete_file no-ops / pre-
    // refuses dirs otherwise and falls through to normal
    // nav). This is the NavTree-focus Ctrl+D; the preview-
    // focus Ctrl+D (half-page scroll) is a separate block.
    if !event.repeat
        && action == Some(Action::DeleteFile)
        && matches!(state.mode, Mode::Files)
        && state.begin_delete_file()
    {
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    nav_action_key(state, key, was_destroy_pending)?;
    Continue(())
}

fn nav_action_key(state: &mut State, key: KeyPress<'_>, was_destroy_pending: Option<WsKey>) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    match action {
        Some(Action::NavDown) => {
            state.tree.move_down();
        }
        Some(Action::NavUp) => {
            state.tree.move_up();
        }
        Some(Action::NavExpand) | Some(Action::NavOpen)
            if !event.repeat =>
        {
            nav_enter_key(state, key)?;
        }
        Some(Action::NavCollapse) if !event.repeat => {
            if !state.collapse_selected_row() {
                if let Some(p) = state.tree.parent_of_selected() {
                    state.tree.selected = p;
                }
            }
        }
        // Mode switches are keymap-driven (mode.files /
        // mode.modules / mode.sessions / mode.hosts) via
        // match guards, so the default single-char chords
        // (f/m/s/h) stay literal text everywhere else — this
        // arm only runs inside the nav-focus match.
        Some(Action::ModeFiles) if !event.repeat =>
        {
            state.enter_mode(Mode::Files);
        }
        Some(Action::ModeModules) if !event.repeat =>
        {
            state.enter_mode(Mode::Modules);
        }
        Some(Action::ModeSessions) if !event.repeat =>
        {
            state.enter_mode(Mode::Sessions);
        }
        // C2 pin-and-leave: `p` toggles pin on the
        // cursor row. Only meaningful in Files mode
        // — `toggle_pin` filters rows whose id
        // doesn't start with `files:`.
        Some(Action::TogglePin) if !event.repeat => {
            state.toggle_pin();
        }
        // ADR 0015 — `h` enters Mode::Hosts, populating
        // the nav tree from `conns`. No backend
        // round-trip needed: the `--dial` set is
        // resolved at startup and lives entirely on
        // the frontend side. Cursor on the
        // currently-selected host is the natural way in.
        Some(Action::ModeHosts) if !event.repeat =>
        {
            state.enter_mode(Mode::Hosts);
        }
        // `.` toggles hidden dotfiles in Files mode
        // (nav-focus-gated via the keymap so it stays
        // literal text in the pty/editor/prompts). Sends
        // nav.toggle_hidden + re-fetches the files tree.
        Some(Action::ToggleHidden) if !event.repeat =>
        {
            if state.workspace_picker.is_some() {
                state.picker_toggle_hidden();
            } else {
                state.toggle_hidden_files();
            }
        }
        // Capital D (Shift+d) in Sessions mode →
        // destroy the cursor row's workspace. Two-
        // press confirm via `was_destroy_pending`:
        // first press arms with the target id, the
        // status line tells the user; second press
        // on the same row fires `workspace.destroy`.
        // Cursor move, mode switch, or any other
        // key clears the arm. A default row ends its
        // run and keeps the row (backend-side —
        // see `WorkspaceDestroyed`'s `kept` branch).
        Some(Action::SessionDestroy) if !event.repeat =>
        {
            session_destroy_key(state, was_destroy_pending)?;
        }
        _ => nav_file_key(state, key)?,
    }
    Continue(())
}

fn nav_enter_key(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { action, .. } = key;
    // Enter in Sessions mode dispatches to the
    // right action based on row kind:
    //   session_create → open the label prompt (B4)
    //   session / pane → attach BL to that session (B3)
    //   anything else  → fall through to expand
    // Right keeps the pure-expand behaviour so
    // users can explore the panes list without
    // re-targeting the BL pane.
    let is_enter =
        action == Some(Action::NavOpen);
    if is_enter && matches!(state.mode, Mode::Hosts) {
        // ADR 0015: persist the selected host
        // so the next launcher run targets
        // it. We don't tear down the live
        // transport — that would require a
        // sentinel-file protocol with the
        // launcher. ADR 0042 L2a: every host is
        // already a live connection, so Enter
        // just navigates the Sessions-mode
        // cursor to that host's node (ADR
        // 0015's relaunch flow is deleted).
        state.pick_host_under_cursor();
        return Break(());
    }
    if is_enter && matches!(state.mode, Mode::Sessions) {
        session_enter_key(state)?;
    }
    state.try_expand_selected();
    Continue(())
}
