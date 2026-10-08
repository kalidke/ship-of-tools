//! Session keys from the tree: the workspace picker, Enter on a Sessions row, and the two-press destroy.

use super::*;
use std::ops::ControlFlow::{self, Break, Continue};
use crate::ui::input::keypress::KeyPress;

pub(in crate::ui) fn picker_key(state: &mut State, key: KeyPress<'_>) -> ControlFlow<()> {
    let KeyPress { event, action, .. } = key;
    // Sessions-mode create-session input (B4): when
    // the prompt is active, key events route into the
    // label buffer instead of the usual nav shortcuts.
    // Enter confirms, Esc cancels, Backspace pops one
    // char, plain Char appends. Other keys ignored
    // (no arrows / no q-to-exit) so the user isn't
    // surprised by mode-switch shortcuts inside what
    // visually looks like text input.
    // Workspace picker is active (ADR 0014). The
    // NavTree key handler routes navigation into the
    // picker's directory tree instead of the regular
    // Sessions list. Up/Down moves cursor; Right
    // drills into the cursored sub-dir; Left/Backspace
    // ascends to parent; Enter commits the cursored
    // directory as the new workspace (with the ccb
    // agent), Shift+Enter commits it as a bare session
    // (no LLM agent); Esc cancels. q is intentionally
    // *not* a quit shortcut here so the user can still
    // type single chars later.
    if state.workspace_picker.is_some() {
        // Up/Down repeats so hold-to-scroll feels
        // right. Enter on the cursored sub-directory
        // is the "this is the one" gesture and commits
        // it as the workspace root (Shift+Enter for a
        // bare, agent-less session). Right is the
        // no-commit preview path (drill in without
        // selecting). Left / Backspace walks back to
        // the parent. Esc cancels.
        // Commit is keymap-driven (.sot/keybindings.toml:
        // session.create / session.create_bare) for no-recompile
        // reconfig. The resolver distinguishes Enter from Shift+Enter.
        if !event.repeat
            && action == Some(Action::SessionCreateCodex)
        {
            state.picker_confirm_selected("codex");
            return Break(());
        }
        if !event.repeat
            && action == Some(Action::SessionCreateBare)
        {
            state.picker_confirm_selected("none");
            return Break(());
        }
        if !event.repeat
            && action == Some(Action::SessionCreate)
        {
            state.picker_confirm_selected("claude");
            return Break(());
        }
        // Per-session accounts (owner-simplified brief,
        // 2026-09-15): Tab cycles the account choice.
        // No-op (via picker_cycle_account) when the
        // choice is hidden (0 or 1 discovered accounts).
        if !event.repeat
            && action == Some(Action::SessionAccountNext)
        {
            state.picker_cycle_account();
            return Break(());
        }
        match action {
            Some(Action::NavDown) => {
                state.picker_cursor_down();
                return Break(());
            }
            Some(Action::NavUp) => {
                state.picker_cursor_up();
                return Break(());
            }
            Some(Action::NavExpand) if !event.repeat => {
                state.picker_drill_in();
                return Break(());
            }
            Some(Action::NavCollapse | Action::PickerParent)
                if !event.repeat =>
            {
                state.picker_ascend();
                return Break(());
            }
            Some(Action::Cancel) if !event.repeat => {
                state.picker_cancel();
                return Break(());
            }
            _ => {
                return Break(());
            }
        }
    }
    Continue(())
}

pub(in crate::ui) fn session_enter_key(state: &mut State) -> ControlFlow<()> {
    let row = state.tree.rows.get(state.tree.selected);
    let kind = row.map(|r| r.node.kind.clone());
    match kind.as_deref() {
        Some("session_create") => {
            let host = row
                .and_then(|r| r.node.payload.get("host"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| state.active_host.clone());
            state.begin_create_session(host);
            return Break(());
        }
        Some("session") | Some("pane") => {
            if let Some(session_name) = state.selected_session_name() {
                let host = state
                    .selected_session_host()
                    .unwrap_or_else(|| state.active_host.clone());
                let listed_spelling = if let Some(id) = row
                    .and_then(|r| r.node.payload.get("workspace_id"))
                    .and_then(|v| v.as_str())
                {
                    if id.is_empty() {
                        state.refuse_result("empty workspace identity");
                        return Continue(());
                    }
                    Some(id.to_string())
                } else if let Some(rows) = state.workspace_lists.get(&host) {
                    let mut matches = rows.iter().filter(|w| w.session_name == session_name);
                    let first = matches.next();
                    if matches.next().is_some() {
                        state.refuse_result("ambiguous attachment target");
                        return Continue(());
                    }
                    first.map(|w| w.workspace_id.clone())
                } else {
                    None
                };
                if let Some(spelling) = listed_spelling {
                    match resolve_listed_workspace(&state.workspace_lists, &host, &spelling) {
                        Ok(target) => state.switch_to_resolved_workspace(target, true),
                        Err(reason) => state.refuse_result(&reason),
                    }
                } else {
                    state.attach_session_to_bl(host, session_name);
                }
            }
        }
        _ => {}
    }
    Continue(())
}

pub(in crate::ui) fn session_destroy_key(state: &mut State, was_destroy_pending: Option<WsKey>) -> ControlFlow<()> {
    let Some(row) = state.tree.rows.get(state.tree.selected) else {
        return Break(());
    };
    if row.node.kind != "session" {
        return Break(());
    }
    let target_id = row
        .node
        .payload
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // ADR 0042 L2a: destroy targets the ROW's
    // own host, not `active_host` — routed via
    // `send_to`.
    let target_host = row
        .node
        .payload
        .get("host")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| state.active_host.clone());
    let target_label = row
        .node
        .payload
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or(row.node.label.as_str())
        .to_string();
    let Some(target_id) = target_id else {
        state.status =
            "destroy: row has no workspace_id (refresh `s` and retry)"
                .to_string();
        state.window.request_redraw();
        return Break(());
    };
    let target: WsKey = (target_host.clone(), target_id.clone());
    if was_destroy_pending.as_ref() == Some(&target) {
        if let Err(e) = state.send_to(
            &target_host,
            crate::net::transport::OutgoingReq::WorkspaceDestroy {
                workspace_id: target_id.clone(),
            },
        ) {
            tracing::warn!(error = %e, "drop workspace.destroy");
            state.status = format!(
                "destroy '{target_label}' failed · channel closed"
            );
        } else {
            state.status = format!("destroying '{target_label}'…");
        }
        state.window.request_redraw();
    } else {
        state.pending_destroy_target = Some(target);
        state.status =
            format!("press {} again to destroy '{target_label}' · any other key cancels", state.bindings.first_label(Action::SessionDestroy));
        state.window.request_redraw();
    }
    Continue(())
}
