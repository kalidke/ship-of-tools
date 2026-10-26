//! Expanding and collapsing the selected row: what each mode's expand asks for, and the collapse.

use super::*;

impl State {
    /// Dispatch an expand request for the currently-selected row, mirroring
    /// what the Enter/Right keyboard arm does. Returns true when a request
    /// was queued; callers can use that to chain (e.g. `--auto-expand`
    /// consuming itself after the first successful fire).
    pub(in crate::ui) fn try_expand_selected(&mut self) -> bool {
        // ADR 0042 L2a: a Sessions-tree host node's children are already
        // in `workspace_lists` — no wire round-trip needed to expand one.
        // Declines (returns false, falls through below) for any other row
        // kind. Checked before `row` is bound below — both need to inspect
        // `self.nav.tree().rows`, and this one also needs `&mut self`.
        if self.try_expand_session_host_local() {
            return true;
        }
        // Mode::Hosts root: children are built only on mode ENTRY
        // (`enter_mode` → `populate_hosts_tree`) — collapsing then
        // re-expanding the root without leaving the mode took the generic
        // `TreeChildren` wire path below, which has no server-side handler
        // for the synthetic "hosts:" id and left the root empty (first
        // live shakedown fix). Same local-expansion seam as
        // `try_expand_session_host_local` above.
        if self.try_expand_hosts_root_local() {
            return true;
        }
        let Some(row) = self.nav.tree().rows().get(self.nav.tree().selected()) else {
            return false;
        };
        if !row.node.has_children || row.expanded {
            return false;
        }
        // ADR 0042 L2a: expanding a `session` row (→ tmux.list_panes) must
        // target THAT row's own host, not `active_host` — the cursor can
        // sit on a non-active host's row without having switched to it.
        // `None` for every other kind means "route via self.send as
        // before" (unchanged behavior).
        let target_host = if row.node.kind == "session" {
            self.selected_session_host()
        } else {
            None
        };
        let outgoing = if row.node.kind == "modules" {
            // Modules-mode root re-expansion: re-request `project.scan`
            // for the whole package tree. `tree.children` is files-mode-
            // only and would error on the synthetic "modules:" id; the
            // entire scan ships in one round-trip anyway so per-row
            // expansion doesn't need a separate fetch.
            let generation =
                self.next_project_scan_gen(self.selected.host().clone(), self.selected.workspace_id().clone());
            Some(crate::net::transport::OutgoingReq::ProjectScan {
                workspace_id: self.selected.workspace_id().clone(),
                generation,
            })
        } else if row.node.kind == "sessions" {
            // Sessions-mode root re-expansion: refresh the workspace
            // registry. Per ADR 0014 the row source is workspace.list,
            // not tmux.list_sessions.
            Some(crate::net::transport::OutgoingReq::WorkspaceList)
        } else if row.node.kind == "module" {
            row.node
                .payload
                .get("path")
                .and_then(|v| v.as_str())
                .map(|p| crate::net::transport::OutgoingReq::FileParse {
                    path: p.to_string(),
                    workspace_id: self.selected.workspace_id().clone(),
                })
        } else if row.node.kind == "function" {
            // Read module + name out of payload (populated when col-2
            // splice built this row). Functions without that payload
            // came from somewhere else — skip rather than guess.
            let module = row
                .node
                .payload
                .get("module")
                .and_then(|v| v.as_str())
                .map(String::from);
            let name = row
                .node
                .payload
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from);
            let ws = self.selected.workspace_id().clone();
            module.zip(name).map(
                |(module, name)| crate::net::transport::OutgoingReq::FunctionMethods {
                    module,
                    name,
                    workspace_id: ws,
                },
            )
        } else {
            Some(crate::net::transport::OutgoingReq::TreeChildren {
                parent_id: row.node.id.clone(),
                workspace_id: self.selected.workspace_id().clone(),
            })
        };
        if let Some(req) = outgoing {
            let sent = match &target_host {
                Some(host) => self.send_to(host, req),
                None => self.send(req),
            };
            if let Err(e) = sent {
                tracing::warn!(target: "sot::ui::nav::tree", error = %e, "drop expand request — channel closed");
                return false;
            }
            // Flip the disclosure at REQUEST time: apply_children now drops
            // replies for collapsed parents (so background refreshes can't
            // reopen a user's collapse), which makes marking here the
            // expand's half of that contract. Rows whose reply path
            // rebuilds the whole view (project.scan / workspace.list) are
            // covered by the rebuilds' own collapse preservation: set_flat
            // suppresses subtrees of currently-collapsed ids and set_root
            // keeps a collapsed same-id root collapsed, so a Left between
            // request and reply wins there too (codex review, round 3).
            self.nav.mark_selected_expanded();
            return true;
        }
        false
    }

    /// Collapse the selected row via TreeView, and — on success — cancel any
    /// pending deep reveal whose target lives UNDER the collapsed row
    /// (codex review, round 3): the walk would otherwise re-expand the very
    /// row the user just closed on its next continuation. The user's
    /// collapse outranks a driven reveal; the preview body already showed.
    pub(in crate::ui) fn collapse_selected_row(&mut self) -> bool {
        let collapsed_id = self
            .nav.tree()
            .rows()
            .get(self.nav.tree().selected())
            .map(|r| r.node.id.clone());
        if !self.nav.collapse_selected() {
            return false;
        }
        if let (Some(id), Some(target)) = (collapsed_id, self.reveal.target()) {
            // A ROOT id is `<mode>:` with an EMPTY rest ("files:") — not
            // merely ends-with-':' (codex round 4: a directory literally
            // named "ab:" yields id "files:ab:", and an ends-with test would
            // let collapsing it cancel a reveal of the SIBLING "files:ab:cd").
            // Deeper ids scope with '/'.
            let is_root = id.split_once(':').is_some_and(|(_, rest)| rest.is_empty());
            let scoped = if is_root {
                target != id.as_str() && target.starts_with(&id)
            } else {
                target.starts_with(&format!("{id}/"))
            };
            if scoped {
                tracing::info!(target: "sot::ui::nav::tree", collapsed = %id, target = %target,
                    "reveal: cancelled by user collapse of an ancestor");
                self.reveal.abort();
            }
        }
        true
    }
}
