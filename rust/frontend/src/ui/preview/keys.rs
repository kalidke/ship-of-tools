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
    png_key(state, key, label)?;
    preview_action_key(state, key, h);
    Continue(())
}

fn page_turn_key(state: &mut State, key: KeyPress<'_>, label: String) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    // Page transport for paginated previews (ADR 0021):
    // n/p and PgDn/PgUp re-fire preview.get for the
    // *shown* node at page ± 1 (clamped). Driven purely
    // by the reply's page extras — the chrome never
    // knows it's a PDF. Consumed even at the clamp edges
    // so a stray press on page 1/N doesn't leak into
    // other handlers; on NON-paginated previews PgUp/
    // PgDn fall through to the text-scroll arms below.
    // No autorepeat: each page is a fresh pdftoppm run.
    if let Some((page, count)) = state.preview_page {
        if count > 1 && !event.repeat {
            {
                let next = match action {
                    Some(Action::PageNext) => Some(page.saturating_add(1).min(count)),
                    Some(Action::PagePrev) => Some(page.saturating_sub(1).max(1)),
                    _ => None,
                };
                if let Some(np) = next {
                    if np != page {
                        if let Some(node_id) =
                            state.preview_node_id_fired.clone()
                        {
                            // New page opens at fit; drop
                            // any pending zoom re-raster.
                            state.preview_page_raster_pending = None;
                            let (fit_w, fit_h) = state.preview_fit_px();
                            let generation = state.next_preview_gen();
                            if let Err(e) = state.send(
                                crate::transport::OutgoingReq::PreviewGet {
                                    node_id,
                                    workspace_id: state
                                        .active_workspace_id
                                        .clone(),
                                    page: Some(np),
                                    fit_w,
                                    fit_h,
                                    generation,
                                },
                            ) {
                                tracing::warn!(error = %e,
                                    "drop page-turn preview.get — channel closed");
                            }
                        }
                    }
                    state.last_key = Some(label);
                    state.window.request_redraw();
                    return Break(());
                }
            }
        }
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
        // here too (it is not a PNG zoom/pan binding, so
        // the block above falls through), and users reach
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
            let sources = &state.preview_md.code_block_sources;
            if !sources.is_empty() {
                let joined = sources.join("\n");
                let n = sources.len();
                match arboard::Clipboard::new()
                    .and_then(|mut cb| cb.set_text(joined))
                {
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
            state.docs_open_external(path);
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

fn preview_scroll_key(state: &mut State, key: KeyPress<'_>, h: i32) {
    let KeyPress { event, action, .. } = key;
    match action {
        Some(Action::ScrollPageUp) => {
            // 1/3-pane step preserves reading
            // context — full-page jumps lost the
            // user's place. Ctrl+u still half-pages
            // for the "I really want to jump"
            // case.
            let page_step = (h / 3).max(1);
            let new = (state.preview_scroll as i32 - page_step).max(0);
            state.preview_scroll = new as u16;
        }
        Some(Action::ScrollPageDown) => {
            let page_step = (h / 3).max(1);
            let new = (state.preview_scroll as i32 + page_step).max(0);
            state.preview_scroll = new as u16;
        }
        // Plain ArrowUp / ArrowDown scroll the
        // markdown preview vertically by one row.
        // PNG previews intercept these earlier
        // (Action::PreviewPngPanUp/Down) so this
        // arm only fires for non-PNG content.
        Some(Action::PreviewUp) => {
            state.preview_scroll = state.preview_scroll.saturating_sub(1);
        }
        Some(Action::PreviewDown) => {
            state.preview_scroll = state.preview_scroll.saturating_add(1);
        }
        Some(Action::PreviewStart) if !event.repeat => {
            state.preview_scroll = 0;
        }
        Some(Action::PreviewEnd) if !event.repeat => {
            // Redraw clamps to (total - visible).
            state.preview_scroll = u16::MAX;
        }
        Some(Action::PreviewHalfUp) => {
            let new = (state.preview_scroll as i32 - h / 2).max(0);
            state.preview_scroll = new as u16;
        }
        Some(Action::PreviewHalfDown) => {
            let new = (state.preview_scroll as i32 + h / 2).max(0);
            state.preview_scroll = new as u16;
        }
        _ => {}
    }
}
