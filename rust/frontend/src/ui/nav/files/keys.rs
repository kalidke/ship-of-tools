//! File keys from the tree: the tree's text prompts and the file row actions (open, docs, download, upload, run).

use super::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

pub(in crate::ui) fn nav_prompt_key(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { event, action, ctrl, alt, super_, .. } = key;
    // NavTree text prompt active (Ctrl+N new-file-or-
    // folder, and future delete-confirm). Like the
    // picker, it steals every keystroke so the user can
    // type a name without nav shortcuts firing: printable
    // chars append (an embedded path separator is
    // rejected at the source; a single trailing `/` is
    // allowed as the "make it a directory" marker),
    // Backspace pops, Enter confirms, Esc cancels, and
    // any other nav key is swallowed so arrows / mode
    // switches don't disturb the tree mid-type.
    if state.nav_prompt.is_some() {
        // ConfirmDelete is a y/N gate, not a text field:
        // 'y'/'Y' confirms, everything else (incl.
        // 'n'/'N'/Esc) cancels. CreateFile keeps its
        // text-input behaviour below — branch on variant.
        if matches!(state.nav_prompt, Some(NavPrompt::ConfirmDelete { .. })) {
            match &event.logical_key {
                _ if action == Some(Action::DeleteConfirm) && !event.repeat =>
                {
                    state.confirm_delete_file();
                    return Break(());
                }
                _ => {
                    // 'n'/'N'/Esc/any other key → cancel.
                    state.cancel_nav_prompt();
                    return Break(());
                }
            }
        }
        match &event.logical_key {
            _ if action == Some(Action::Confirm) && !event.repeat => {
                // Route Enter to whichever text prompt is open.
                if matches!(
                    state.nav_prompt,
                    Some(NavPrompt::ScaleEntry { .. })
                ) {
                    state.confirm_scale_entry();
                } else {
                    state.confirm_create_file();
                }
                return Break(());
            }
            _ if action == Some(Action::Cancel) && !event.repeat => {
                state.cancel_nav_prompt();
                return Break(());
            }
            Key::Named(NamedKey::Backspace) => {
                state.nav_prompt_backspace();
                return Break(());
            }
            Key::Character(s) => {
                // A character key with a modifier other
                // than Shift (Ctrl/Alt/Super) isn't text
                // — swallow it rather than typing the
                // letter. Plain + Shift chars append.
                if !ctrl && !alt && !super_ {
                    for c in s.chars() {
                        state.nav_prompt_push_char(c);
                    }
                }
                return Break(());
            }
            _ => {
                return Break(());
            }
        }
    }
    Continue(())
}

pub(in crate::ui) fn nav_file_key(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    match action {
        // `o` opens the cursored row in an external
        // tool: text/html previews → temp file + OS
        // browser; .jl files → backend `pluto.open`
        // (header-checked on the backend, returns
        // `not_pluto_flavored` for raw .jl). Routed
        // by the cursored row's path, not preview
        // mime — the JuliaSource plugin renders .jl
        // as tokens-JSON.
        Some(Action::OpenExternal) if !event.repeat => {
            let cursored = state.cursored_files_path();
            state.open_path_external(cursored);
        }
        // `W` (Shift+W): open the project's built Documenter
        // site in the OS browser with full CSS/JS/sub-page
        // fidelity (ADR 0024). Backend serves `docs/build`
        // over a forwarded loopback port. Sends the cursored
        // path so a built docs page deep-links; otherwise the
        // backend opens the index. `W` works from any mode.
        Some(Action::OpenDocs) if !event.repeat =>
        {
            let path = state.cursored_files_path().unwrap_or_default();
            state.docs_open_external(path);
        }
        // `O` (Shift+O): full render WITH code execution
        // for a cursored `.qmd`, then open in the browser.
        // Slower + needs the language kernels on the backend
        // host; `o` is the fast no-execute path.
        Some(Action::OpenExecute) if !event.repeat =>
        {
            let cursored = state.cursored_files_path();
            state.quarto_open_execute(cursored);
        }
        // `d`: download the cursored file row to the local
        // OS downloads dir (OS-independent), non-clobbering.
        // Transport streams chunks; dir rows are a no-op.
        Some(Action::Download) if !event.repeat =>
        {
            state.start_download();
        }
        // `u`: pick a local file via the native OS dialog
        // and upload it to the cursored nav folder (the dir
        // itself for a dir row, else the file's parent).
        Some(Action::Upload) if !event.repeat =>
        {
            state.start_upload();
        }
        // Priority J: `r` resets the workspace's
        // persistent REPL into the file's closest-
        // ancestor Project.toml then include()s the
        // file. `R` (Shift+r) just include()s in the
        // existing REPL — no env change. Both gate
        // on a `.jl` cursored row; non-.jl rows are
        // a no-op. Output flows back through the
        // existing repl frame stream into the REPL
        // drawer. Future: mirror the last image
        // frame to the preview pane (TODO row 161).
        Some(Action::RunFresh | Action::RunCurrent) if !event.repeat =>
        {
            run_file_key(state, key)?;
        }
        _ => {}
    }
    Continue(())
}

fn run_file_key(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { action, .. } = key;
    let Some(abs) = state.cursored_files_path() else {
        return Break(());
    };
    if !abs.ends_with(".jl") {
        tracing::debug!(path = %abs,
            "`r`/`R` ignored — not a .jl file");
        return Break(());
    }
    let fresh = action == Some(Action::RunFresh);
    let basename = abs
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(abs.as_str())
        .to_string();
    // `r` resets the REPL *process* on the backend
    // (fresh `julia --project=…`), so reset the
    // drawer window to match — the old scrollback
    // belongs to a now-dead session. `R` keeps the
    // existing session and its scrollback. History
    // derives from `repl_log`, so clearing the log
    // clears it too; the eval counter keeps
    // monotonically rising to avoid eval_id reuse
    // with any still-draining replies.
    if fresh {
        state.repl_log.clear();
        state.repl_scroll = 0;
        state.repl_pkg_mode = false;
        state.history_pos = None;
        state.history_saved = None;
    }
    // Pre-register a `repl_log` entry exactly the way
    // `submit_repl_input` does for repl.eval, so the
    // ReplRunFileDone reply can splice frames in by
    // eval_id and the drawer scrollback shows the
    // run's output alongside everything else.
    state.repl_eval_counter = state.repl_eval_counter.saturating_add(1);
    let eval_id = state.repl_eval_counter;
    let owner_host = state.active_host.clone();
    let workspace_key = state.active_ws_key();
    state
        .eval_id_workspace
        .insert((owner_host, eval_id), workspace_key.clone());
    if state.repl_log.len() >= 256 {
        let excess = state.repl_log.len() - 255;
        state.repl_log.drain(0..excess);
    }
    let synthetic_code = format!("{} {}", if fresh { "r" } else { "R" }, abs);
    state.repl_log.push(ReplEntry {
        eval_id,
        code: synthetic_code,
        frames: Vec::new(),
        elapsed_ms: 0,
        in_flight: true,
        pkg_mode: false,
        origin: None,
    });
    if let Err(e) =
        state.send(crate::net::transport::OutgoingReq::ReplRunFile {
            eval_id,
            path: abs.clone(),
            fresh,
            workspace_id: state.active_workspace_id.clone(),
        })
    {
        tracing::warn!(error = %e,
            "failed to dispatch repl.run_file");
        if let Some(entry) =
            state.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
        {
            entry.in_flight = false;
            entry.frames.push(sot_protocol::ReplFrame::Error {
                message: format!("transport channel closed: {e}"),
                stacktrace: Vec::new(),
            });
        }
        state.status = format!(
            "repl.run_file '{basename}' failed · channel closed"
        );
    } else if fresh {
        state.status =
            format!("running '{basename}' (resetting REPL …)");
    } else {
        state.status = format!("running '{basename}' (existing repl)");
    }
    auto_open_repl_drawer(state);
    state.window.request_redraw();
    Continue(())
}

fn auto_open_repl_drawer(state: &mut State) {
    // Auto-open (or switch to) the REPL drawer so
    // the run's output is visible (settings-gated,
    // default on). If the Terminal drawer is up we
    // swap it for the REPL since that's where the
    // output lands. Keep NavTree focus so `r`/`R`
    // stay usable — unlike Ctrl+J this does not
    // steal focus.
    if state.settings.repl_auto_open_drawer_on_run
        && state.drawer != DrawerContent::Repl
        && state
            .settings
            .resolve_preset(state.monitor_aspect)
            .drawer
            .is_some()
    {
        state.drawer = DrawerContent::Repl;
    }
}
