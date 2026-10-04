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
            if let Some(session_name) =
                state.selected_session_name()
            {
                // ADR 0014: route the swap
                // through the unified entry
                // point. The slug is the
                // session name with the
                // `sot-be-` prefix
                // stripped (the backend's
                // resolve() accepts either
                // a workspace_id or a slug).
                let slug = session_name
                    .strip_prefix("sot-be-")
                    .map(|s| s.to_string());
                if slug.is_some() {
                    // ADR 0042 L2a: the
                    // cursored row's OWN
                    // host — both `session`
                    // and `pane` rows carry
                    // `payload.host`,
                    // stamped at the reply
                    // that built them.
                    let host = state
                        .selected_session_host()
                        .unwrap_or_else(|| {
                            state.active_host.clone()
                        });
                    // Sessions-Enter is
                    // person-driven: clear
                    // this row's blue.
                    state.switch_to_workspace(
                        host,
                        slug,
                        Some(session_name),
                        true,
                    );
                } else {
                    // Foreign tmux session
                    // surfaced by an older
                    // backend that hadn't
                    // filtered them out —
                    // just retarget BL, on
                    // the cursored row's
                    // OWN host (this row
                    // was never switched
                    // to, so active_host
                    // alone would be wrong
                    // — same reasoning as
                    // the branch above).
                    let host = state
                        .selected_session_host()
                        .unwrap_or_else(|| {
                            state.active_host.clone()
                        });
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
            crate::transport::OutgoingReq::WorkspaceDestroy {
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
