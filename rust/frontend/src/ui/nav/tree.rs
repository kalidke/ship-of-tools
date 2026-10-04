//! The tree view: rows, expand and collapse, and how replies merge into it.

use super::*;

/// One visible row in the Files-mode tree. The flat-list layout is the chrome
/// abstraction: ratatui renders top-to-bottom, and indent + disclosure char
/// communicate depth and expansion state. Collapsing a row drops every
/// strictly-deeper row beneath it; re-expanding re-fetches via `tree.children`
/// rather than caching, since the spike has no staleness story yet.
#[derive(Clone)]
pub(in crate::ui) struct TreeRow {
    pub(in crate::ui) node: TreeNode,
    pub(in crate::ui) depth: usize,
    pub(in crate::ui) expanded: bool,
}

/// Group a drained subtree run (rows strictly deeper than the parent) into
/// per-direct-child buckets: id -> (kind, expanded, descendant rows). Shared
/// by every merge-preserving rebuild (apply_children, set_root) so they all
/// apply the same compatible-container rule.
fn harvest_child_state(
    old: Vec<TreeRow>,
    child_depth: usize,
) -> HashMap<String, (String, bool, Vec<TreeRow>)> {
    let mut saved: HashMap<String, (String, bool, Vec<TreeRow>)> = HashMap::new();
    let mut cur: Option<(String, String, bool, Vec<TreeRow>)> = None;
    for row in old {
        if row.depth == child_depth {
            if let Some((id, kind, ex, desc)) = cur.take() {
                saved.insert(id, (kind, ex, desc));
            }
            cur = Some((
                row.node.id.clone(),
                row.node.kind.clone(),
                row.expanded,
                Vec::new(),
            ));
        } else if let Some((_, _, _, desc)) = cur.as_mut() {
            desc.push(row);
        }
    }
    if let Some((id, kind, ex, desc)) = cur.take() {
        saved.insert(id, (kind, ex, desc));
    }
    saved
}

/// Rebuild a children run in the NEW reply's order, re-attaching each
/// surviving child's saved (expanded, descendants) — but only when the fresh
/// node is still a compatible container (same kind, has_children): an
/// expanded directory replaced by a same-named file arrives as a plain leaf,
/// never resurrecting ghost descendants.
fn rebuild_children_preserving(
    saved: &mut HashMap<String, (String, bool, Vec<TreeRow>)>,
    children: Vec<TreeNode>,
    child_depth: usize,
) -> Vec<TreeRow> {
    let mut rebuilt: Vec<TreeRow> = Vec::with_capacity(children.len());
    for c in children {
        let (expanded, desc) = match saved.remove(&c.id) {
            Some((kind, ex, desc)) if kind == c.kind && c.has_children => (ex, desc),
            _ => (false, Vec::new()),
        };
        rebuilt.push(TreeRow {
            node: c,
            depth: child_depth,
            expanded,
        });
        rebuilt.extend(desc);
    }
    rebuilt
}

/// Drop the incoming subtree rows of every id the user is currently showing
/// COLLAPSED, keeping (and re-collapsing) the container row itself. Applied
/// by whole-tree rebuild paths (set_flat) so a reply that races a user
/// collapse — the modules root Right-then-Left case — cannot reopen it
/// (codex review, round 3).
fn suppress_collapsed_subtrees(
    rows: Vec<TreeRow>,
    collapsed: &std::collections::HashSet<String>,
) -> Vec<TreeRow> {
    if collapsed.is_empty() {
        return rows;
    }
    let mut out: Vec<TreeRow> = Vec::with_capacity(rows.len());
    let mut skip_deeper: Option<usize> = None;
    for mut row in rows {
        if let Some(d) = skip_deeper {
            if row.depth > d {
                continue;
            }
            skip_deeper = None;
        }
        if collapsed.contains(&row.node.id) {
            row.expanded = false;
            skip_deeper = Some(row.depth);
        }
        out.push(row);
    }
    out
}

#[derive(Clone, Default)]
pub(in crate::ui) struct TreeView {
    pub(in crate::ui) rows: Vec<TreeRow>,
    pub(in crate::ui) selected: usize,
    /// Bumped on every CONTENT mutation (set_root / set_flat /
    /// apply_children). The nav-spill trigger compares it across frames to
    /// tell a USER cursor move (selected changed, content same) from a
    /// content change that MOVED the cursor under the user (initial async
    /// tree load, cursor restore, a refresh splicing rows above the
    /// cursor) — only the former arms the spill (codex review: the old
    /// first-frame-only boot suppression made startup spill
    /// timing-dependent).
    pub(in crate::ui) generation: u64,
}

impl TreeView {
    pub(in crate::ui) fn new() -> Self {
        Self {
            rows: Vec::new(),
            selected: 0,
            generation: 0,
        }
    }

    /// Capture the node id of the currently-selected row, if any. Used by
    /// row-mutating ops to re-anchor the cursor by node id across a
    /// wipe-and-refill so a transport reconnect / project.scan / stale
    /// tree.children reply doesn't kick the user back to row 0.
    fn selected_node_id(&self) -> Option<String> {
        self.rows.get(self.selected).map(|r| r.node.id.clone())
    }

    /// Replace the entire flat row list at once. Used by ops that
    /// deliver a fully-nested tree in one shot (today: `project.scan`)
    /// so the chrome doesn't have to walk `set_root` + per-level
    /// `apply_children` calls. Preserves the cursor on its previous node
    /// when that node is still present in the new rows; falls back to 0
    /// when the previously-cursored node is gone.
    pub(in crate::ui) fn set_flat(&mut self, rows: Vec<TreeRow>) {
        self.generation = self.generation.wrapping_add(1);
        let prev_id = self.selected_node_id();
        let prev_selected = self.selected;
        let prev_len = self.rows.len();
        // Whole-tree rebuilds must not reopen what the user closed: every
        // container currently showing collapsed keeps its collapse, its
        // incoming subtree dropped (the next expand re-fetches). Covers the
        // Right-then-Left-before-reply race on rebuild-path roots (codex
        // review, round 3).
        let collapsed: std::collections::HashSet<String> = self
            .rows
            .iter()
            .filter(|r| r.node.has_children && !r.expanded)
            .map(|r| r.node.id.clone())
            .collect();
        self.rows = suppress_collapsed_subtrees(rows, &collapsed);
        self.selected = prev_id
            .as_deref()
            .and_then(|id| self.rows.iter().position(|r| r.node.id == id))
            .unwrap_or(0);
        tracing::info!(
            prev_id = ?prev_id,
            prev_selected,
            prev_len,
            new_selected = self.selected,
            new_len = self.rows.len(),
            "TreeView::set_flat"
        );
    }

    /// Re-seed the view from a `tree.root` reply. Preserves the cursor on
    /// its previous node when that node is still present under the new
    /// root; falls back to 0 when the previously-cursored node is gone
    /// (different root, deleted, etc.).
    pub(in crate::ui) fn set_root(&mut self, root: TreeNode, children: Vec<TreeNode>) {
        self.generation = self.generation.wrapping_add(1);
        let prev_id = self.selected_node_id();
        let prev_selected = self.selected;
        let prev_len = self.rows.len();
        // Re-seeding the SAME root (Sessions-mode refresh, enter-mode
        // refreshes) is a merge, not a wipe (codex review, round 3):
        // surviving children keep their expansion + shown descendants — so
        // a workspace.list reply racing a session-row expand no longer
        // clears the request-time expanded mark (which made the following
        // panes reply drop) — and a user-collapsed root STAYS collapsed
        // (children dropped; the next expand re-fetches). A DIFFERENT root
        // id is a genuine re-seed (workspace switch): fresh rows.
        let same_root = self
            .rows
            .first()
            .map(|r| r.node.id == root.id)
            .unwrap_or(false);
        let root_collapsed = same_root && !self.rows[0].expanded;
        let mut saved = if same_root {
            let old: Vec<TreeRow> = self.rows.drain(1..).collect();
            harvest_child_state(old, 1)
        } else {
            HashMap::new()
        };
        // The root is open unless the USER closed it. An EMPTY listing is
        // still an open folder: seeding it collapsed made a fresh/empty
        // project deaf to every live refresh (`refresh_tree_dir_if_expanded`
        // and `apply_children` both require an expanded parent), and the
        // same-root re-seed above then inherited that collapse forever —
        // the first files created in a new project never appeared until a
        // manual expand (owner report, 2026-09-14).
        let mut rows = vec![TreeRow {
            expanded: !root_collapsed,
            node: root,
            depth: 0,
        }];
        if !root_collapsed {
            rows.extend(rebuild_children_preserving(&mut saved, children, 1));
        }
        self.rows = rows;
        self.selected = prev_id
            .as_deref()
            .and_then(|id| self.rows.iter().position(|r| r.node.id == id))
            .unwrap_or(0);
        tracing::info!(
            prev_id = ?prev_id,
            prev_selected,
            prev_len,
            new_selected = self.selected,
            new_len = self.rows.len(),
            "TreeView::set_root"
        );
    }

    /// Merge the fresh children of `parent_id` into the flat list: the
    /// parent's listing is replaced (new names appear, vanished names drop,
    /// order follows the reply), but every surviving child that was
    /// expanded — and is still a compatible container — keeps its expansion
    /// AND its previously-shown descendant rows. Preserves the cursor on
    /// its previous node by node-id lookup — with the merge, a cursor on a
    /// NESTED node under a surviving child now survives refreshes too; only
    /// a cursor whose node truly vanished falls back to the parent row.
    ///
    /// Contract change (codex review): this NEVER expands the parent. A
    /// reply for a collapsed parent is dropped, so intentional expands mark
    /// their row `expanded` at REQUEST time; background refreshes can then
    /// never reopen something the user closed.
    pub(in crate::ui) fn apply_children(&mut self, parent_id: &str, children: Vec<TreeNode>) {
        let Some((pidx, pdepth)) = self.rows.iter().enumerate().find_map(|(i, r)| {
            if r.node.id == parent_id {
                Some((i, r.depth))
            } else {
                None
            }
        }) else {
            tracing::debug!(%parent_id, "tree.children reply for unknown parent — ignoring");
            return;
        };
        // A reply for a parent the user has since COLLAPSED is dropped
        // (codex review, collapse-race): apply_children no longer force-
        // opens anything. Every INTENTIONAL expand marks its row expanded
        // at request time (try_expand_selected / the reveal ancestor walk),
        // so a collapsed parent here means a background refresh raced a
        // user collapse — honoring the collapse wins; the listing is
        // re-fetched fresh on the next expand anyway. Gate BEFORE the
        // spill generation bump: a dropped reply changes no content.
        if !self.rows[pidx].expanded {
            tracing::debug!(%parent_id, "tree.children reply for collapsed parent — dropping");
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let prev_id = self.selected_node_id();
        let mut end = pidx + 1;
        while end < self.rows.len() && self.rows[end].depth > pdepth {
            end += 1;
        }
        // MERGE, don't wipe (2026-08-18 regression fix): the old drain-and-
        // reinsert collapsed every expanded subtree under the parent and
        // kicked a nested cursor to the parent row. Harmless while the
        // watcher path was mostly dead (the wrong-root bug dropped its
        // events), it became a live grenade once PR #101 made events flow:
        // any root-level file event — editor temp-file churn included —
        // reset the whole nav ("nav pane keeps resetting", owner report,
        // caught in the receipt log). Now: save each departing direct
        // child's (kind, expanded, descendant rows), rebuild in the NEW
        // children's order, and re-attach the saved subtree under every
        // surviving id. Vanished children's subtrees drop; new children
        // arrive collapsed; a cursor on a surviving nested node keeps its
        // node (re-anchored by id below).
        let old: Vec<TreeRow> = self.rows.drain((pidx + 1)..end).collect();
        let child_depth = pdepth + 1;
        let mut saved = harvest_child_state(old, child_depth);
        let rebuilt = rebuild_children_preserving(&mut saved, children, child_depth);
        let tail = self.rows.split_off(pidx + 1);
        self.rows.extend(rebuilt);
        self.rows.extend(tail);
        let prev_selected = self.selected;
        self.selected = prev_id
            .as_deref()
            .and_then(|id| self.rows.iter().position(|r| r.node.id == id))
            .unwrap_or(pidx);
        tracing::info!(
            parent_id,
            prev_id = ?prev_id,
            prev_selected,
            pidx,
            new_selected = self.selected,
            new_len = self.rows.len(),
            "TreeView::apply_children"
        );
    }

    pub(in crate::ui) fn move_down(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    pub(in crate::ui) fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Try to collapse the currently-selected row in place. Returns true if
    /// anything happened, so callers can decide whether `Left` should fall
    /// through to "move to parent" instead.
    pub(in crate::ui) fn collapse_selected(&mut self) -> bool {
        let idx = self.selected;
        if idx >= self.rows.len() || !self.rows[idx].expanded {
            return false;
        }
        let depth = self.rows[idx].depth;
        let mut end = idx + 1;
        while end < self.rows.len() && self.rows[end].depth > depth {
            end += 1;
        }
        self.rows.drain((idx + 1)..end);
        self.rows[idx].expanded = false;
        true
    }

    /// Index of the row whose subtree contains the current selection, if any.
    /// O(n) walk back to the first row at a strictly-lesser depth.
    pub(in crate::ui) fn parent_of_selected(&self) -> Option<usize> {
        let idx = self.selected;
        if idx == 0 {
            return None;
        }
        let depth = self.rows[idx].depth;
        if depth == 0 {
            return None;
        }
        (0..idx).rev().find(|&i| self.rows[i].depth < depth)
    }
}

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
        // `self.tree.rows`, and this one also needs `&mut self`.
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
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
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
                self.next_project_scan_gen(self.active_host.clone(), self.active_workspace_id.clone());
            Some(crate::transport::OutgoingReq::ProjectScan {
                workspace_id: self.active_workspace_id.clone(),
                generation,
            })
        } else if row.node.kind == "sessions" {
            // Sessions-mode root re-expansion: refresh the workspace
            // registry. Per ADR 0014 the row source is workspace.list,
            // not tmux.list_sessions.
            Some(crate::transport::OutgoingReq::WorkspaceList)
        } else if row.node.kind == "module" {
            row.node
                .payload
                .get("path")
                .and_then(|v| v.as_str())
                .map(|p| crate::transport::OutgoingReq::FileParse {
                    path: p.to_string(),
                    workspace_id: self.active_workspace_id.clone(),
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
            let ws = self.active_workspace_id.clone();
            module.zip(name).map(
                |(module, name)| crate::transport::OutgoingReq::FunctionMethods {
                    module,
                    name,
                    workspace_id: ws,
                },
            )
        } else {
            Some(crate::transport::OutgoingReq::TreeChildren {
                parent_id: row.node.id.clone(),
                workspace_id: self.active_workspace_id.clone(),
            })
        };
        if let Some(req) = outgoing {
            let sent = match &target_host {
                Some(host) => self.send_to(host, req),
                None => self.send(req),
            };
            if let Err(e) = sent {
                tracing::warn!(error = %e, "drop expand request — channel closed");
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
            if let Some(r) = self.tree.rows.get_mut(self.tree.selected) {
                r.expanded = true;
            }
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
            .tree
            .rows
            .get(self.tree.selected)
            .map(|r| r.node.id.clone());
        if !self.tree.collapse_selected() {
            return false;
        }
        if let (Some(id), Some(target)) = (collapsed_id, self.pending_reveal.as_ref()) {
            // A ROOT id is `<mode>:` with an EMPTY rest ("files:") — not
            // merely ends-with-':' (codex round 4: a directory literally
            // named "ab:" yields id "files:ab:", and an ends-with test would
            // let collapsing it cancel a reveal of the SIBLING "files:ab:cd").
            // Deeper ids scope with '/'.
            let is_root = id.split_once(':').is_some_and(|(_, rest)| rest.is_empty());
            let scoped = if is_root {
                target != &id && target.starts_with(&id)
            } else {
                target.starts_with(&format!("{id}/"))
            };
            if scoped {
                tracing::info!(collapsed = %id, target = %target,
                    "reveal: cancelled by user collapse of an ancestor");
                self.pending_reveal = None;
                self.reveal_awaiting = None;
                self.reveal_refetched = None;
            }
        }
        true
    }
}

/// Render one TreeRow as a single chrome line: cursor caret, depth indent,
/// disclosure char, label, optional pin sigil. Width is fixed-cell ASCII so
/// the per-row run-length stays predictable for the chrome backend's text
/// projection. `pinned` adds a ` ★` suffix so the user can spot the pinned
/// node even when the cursor is elsewhere; the cell-count stays ASCII so
/// no chrome layout math breaks.
pub(in crate::ui) fn format_tree_row(row: &TreeRow, selected: bool, pinned: bool) -> String {
    let caret = if selected { '>' } else { ' ' };
    let disclosure = if row.node.has_children {
        if row.expanded {
            'v'
        } else {
            '>'
        }
    } else {
        '.'
    };
    let indent = "  ".repeat(row.depth);
    let suffix = if pinned { " *" } else { "" };
    format!("{caret} {indent}{disclosure} {}{suffix}", row.node.label)
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
