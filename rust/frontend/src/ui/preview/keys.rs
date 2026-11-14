//! What a key does in the preview pane, in order: the editor, page turns, the image, then actions and scroll.

use crate::ui::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;
use crate::ui::preview::editor::keys::{editor_key, enter_editor};
use crate::ui::preview::image::keys::png_key;

pub(in crate::ui) fn preview_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    editor_key(state, key)?;
    page_turn_key(state, key, label.clone())?;
    // Esc → tree; PgUp/PgDn / Ctrl+u / Ctrl+d /
    // Home / End scroll the preview's flowed text.
    // Held keys repeat for hold-to-scroll. Viewport
    // size is taken from the chrome cell height of
    // the pane — close enough to a body line for
    // the user not to notice the small mismatch
    // with the mouse-wheel row math, and it keeps
    // all four panes on the same rule.
    let h = state.pane_rects.preview.height as i32;
    crate::ui::preview::image::ink_input::ink_key(state, key, label.clone())?;
    png_key(state, key, label)?;
    preview_action_key(state, key, h);
    Continue(())
}

fn page_turn_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    if super::requests::continue_page_key(state, key.action) {
        state.last_key = Some(label);
        state.window.request_redraw();
        return Break(());
    }
    Continue(())
}

fn preview_action_key(state: &mut State, key: KeyPress<'_>, h: i32) {
    let KeyPress { event, action, .. } = key;
    match action {
        Some(Action::ReturnNav) if !event.repeat => {
            state.set_focus(PaneFocus::NavTree);
        }
        // ADR 0022: `c` captures the visible image ROI and
        // sends it to the LLM pane. `capture_roi` no-ops
        // with a status hint when the preview isn't a
        // croppable image.
        //
        // Deliberately unguarded on modifiers: Ctrl+C lands
        // here too, and users reach
        // for the universal copy chord out of habit. Both
        // spellings are the same action and both move focus
        // to the LLM pane once the crop paste lands — the
        // focus move itself lives in the ImageCropped arm,
        // not here, because the crop is async and may fail.
        Some(Action::CaptureRegion) if !event.repeat => {
            state.capture_roi();
        }
        // `e` enters edit mode for the cursored
        // annotation, if there is one. Per the
        // 2026-05-15T21:32Z spec: modal text input,
        // minimal scope, no auto-clobber on save.
        // `y` (vim "yank") copies fenced code blocks in
        // the current markdown preview to the system
        // clipboard. Multiple blocks are joined with a
        // blank line so a "copy everything" call still
        // pastes cleanly into another editor. No-op
        // when the preview isn't markdown or carries no
        // code blocks.
        Some(Action::CopyCode) if !event.repeat => {
            let sources = &state.shaped.md().code_block_sources;
            if !sources.is_empty() {
                let joined = sources.join("\n");
                let n = sources.len();
                match crate::ui::input::write_clipboard_text(joined) {
                    Ok(()) => tracing::info!(
                        blocks = n,
                        "yanked code block(s) to clipboard"
                    ),
                    Err(e) => tracing::warn!(
                        error = %e,
                        "failed to write code blocks to clipboard"
                    ),
                }
            }
        }
        Some(Action::PreviewCopySelection) if !event.repeat => {
            if let Some(status) = copy_selection_status(&state.shaped, |text| {
                crate::ui::input::write_clipboard_text(text).map_err(|e| e.to_string())
            }) {
                state.status.set(status);
            }
        }
        // Open-style keys work from the preview pane too
        // (same handlers as NavTree), acting on the file
        // whose preview is SHOWING — pinned/badge-consumed
        // previews can differ from the nav cursor — with
        // fallback to the cursored row.
        Some(Action::OpenExternal) if !event.repeat => {
            let shown = state
                .previewed_files_path()
                .or_else(|| state.cursored_files_path());
            state.open_path_external(shown);
        }
        Some(Action::OpenDocs) if !event.repeat =>
        {
            let path = state
                .previewed_files_path()
                .or_else(|| state.cursored_files_path())
                .unwrap_or_default();
            state.docs_open_external(&state.selected.host().clone(), path);
        }
        Some(Action::OpenExecute) if !event.repeat =>
        {
            let shown = state
                .previewed_files_path()
                .or_else(|| state.cursored_files_path());
            state.quarto_open_execute(shown);
        }
        Some(Action::EditFile) if !event.repeat => {
            enter_editor(state);
        }
        _ => preview_scroll_key(state, key, h),
    }
}

/// The same clipboard operation as CopyCode, with no clipboard or status effect for an empty selection.
fn copy_selection_status(
    shaped: &crate::ui::preview::shaped::ShapedPreview,
    clipboard: impl FnOnce(String) -> std::result::Result<(), String>,
) -> Option<String> {
    let text = shaped.selected_text()?;
    Some(match clipboard(text) {
        Ok(()) => "copied preview selection".into(),
        Err(error) => {
            tracing::warn!(%error, "failed to copy preview selection");
            format!("copy failed · {error}")
        }
    })
}

fn preview_scroll_key(state: &mut State, key: KeyPress<'_>, h: i32) {
    let KeyPress { event, action, .. } = key;
    let (now, length) = state.scroll_ask();
    match action {
        Some(Action::ScrollPageUp) => {
            // 1/3-pane step preserves reading
            // context — full-page jumps lost the
            // user's place. Ctrl+u still half-pages
            // for the "I really want to jump"
            // case.
            let page_step = (h / 3).max(1);
            state.shaped.scroll_by(-page_step, now, length);
        }
        Some(Action::ScrollPageDown) => {
            let page_step = (h / 3).max(1);
            state.shaped.scroll_by(page_step, now, length);
        }
        // Plain ArrowUp / ArrowDown scroll the
        // markdown preview vertically by one row.
        // PNG previews intercept these earlier
        // (Action::PreviewPngPanUp/Down) so this
        // arm only fires for non-PNG content.
        Some(Action::PreviewUp) => {
            state.shaped.scroll_by(-1, now, length);
        }
        Some(Action::PreviewDown) => {
            state.shaped.scroll_by(1, now, length);
        }
        Some(Action::PreviewStart) if !event.repeat => {
            state.shaped.scroll_to_top(now, length);
        }
        Some(Action::PreviewEnd) if !event.repeat => {
            // Redraw clamps to (total - visible).
            state.shaped.scroll_to_end(now, length);
        }
        Some(Action::PreviewHalfUp) => {
            state.shaped.scroll_by(-(h / 2), now, length);
        }
        Some(Action::PreviewHalfDown) => {
            state.shaped.scroll_by(h / 2, now, length);
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
