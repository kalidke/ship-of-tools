//! The nav mode and the per-(mode, scope) tree store: which tree is active and where the others wait.

use super::*;

/// Which root tree the left pane is showing. Files mode → backend's files
/// hierarchy via `tree.root {mode: "files"}`; Sessions mode → backend
/// tmux registry (ADR 0013) via `tmux.list_sessions`. Cursor position IS
/// preserved across switches: each (mode, scope) keeps its own tree in
/// `TreeStore`, and `enter_mode` swaps the parked view (cursor, expansion,
/// scroll) back in while the refetch refreshes it in place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(in crate::ui) enum Mode {
    #[default]
    Files,
    Modules,
    Sessions,
    /// ADR 0042 L2a — live connected/unreachable status list for every
    /// dialed connection (superseded ADR 0015's "pick one, persist
    /// `last_host`, Ctrl+Q + relaunch": every host is already a live
    /// connection, nothing to relaunch into). Enter moves the
    /// Sessions-mode cursor to the picked host's node
    /// (`pick_host_under_cursor`).
    Hosts,
}

impl Mode {
    pub(in crate::ui) fn label(self) -> &'static str {
        match self {
            Mode::Files => "files",
            Mode::Modules => "modules",
            Mode::Sessions => "sessions",
            Mode::Hosts => "hosts",
        }
    }
}

/// The mode a launch opens in: `--start-mode` if given, else the persisted
/// `last_mode` (B5), else Files. A `harness` run ignores the persisted mode:
/// harness runs must be deterministic, as for the workspace restore.
pub(in crate::ui) fn initial_mode(cli: Option<&str>, persisted: Option<&str>, harness: bool) -> Mode {
    match cli.or(persisted.filter(|_| !harness)) {
        Some("modules") => Mode::Modules,
        Some("sessions") => Mode::Sessions,
        Some("hosts") => Mode::Hosts,
        _ => Mode::Files,
    }
}

// ---------- Tree-provenance redesign: per-(mode, scope) tree storage ----------
//
// The nav tree used to be ONE shared mutable `TreeView` reused across every
// (workspace × mode) combination, with provenance (`files_tree_workspace`)
// side-stamped onto only the Files loader and staleness guards added
// piecemeal to individual consumers — seven v0.4.3 review rounds each found
// the next unguarded path. `TreeStore` retires the class structurally:
// every tree belongs to a `(Mode, TreeScope)` key, installs route by the
// REPLY's key, and a reply for a non-active key lands in its own stored
// slot instead of clobbering the active view.
//
// `State.tree` remains the ACTIVE slot (the ~97 cursor/read sites are
// untouched and always see the active (mode, workspace)'s tree — by
// construction, not by per-consumer guards); the store holds only
// NON-active slots. Every mode/workspace change goes through
// `swap_active_tree`, the single stash/load seam.

/// Workspace half of a tree key. `Workspace` carries the host-qualified
/// key (ADR 0042 L2a, Codex review PR #163: a bare slug collided across
/// hosts — switching from "sot" on host A to "sot" on host B early-returned
/// in `swap_active_tree` on an equal key and left A's tree showing under
/// B's context). The second element is the SAME normalized literal
/// `State::current_workspace_key` has always produced (`"<default>"` for
/// the daemon-default workspace), just paired with the host now, so
/// reply-keying and active-keying still can never disagree about the
/// default workspace's identity WITHIN a host. Sessions and Hosts trees
/// are machine-level, not project-level, so they key as `Global` and
/// survive workspace switches (and host switches) untouched.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::ui) enum TreeScope {
    Workspace(WsKey),
    Global,
}

pub(in crate::ui) type TreeKey = (Mode, TreeScope);

/// The scope a mode's tree lives in: per-workspace for the project-derived
/// trees (Files, Modules), global for the machine-level ones (Sessions,
/// Hosts).
fn mode_scope(mode: Mode, ws_key: &WsKey) -> TreeScope {
    match mode {
        Mode::Files | Mode::Modules => TreeScope::Workspace(ws_key.clone()),
        Mode::Sessions | Mode::Hosts => TreeScope::Global,
    }
}

/// A stored (non-active) tree: the view plus the state that must travel
/// with it.
#[derive(Default)]
pub(in crate::ui) struct TreeSlot {
    view: TreeView,
    /// Nav-pane scroll offset — restoring a tree with someone else's
    /// scroll reads as a viewport jump.
    scroll: u16,
    /// Modules-mode only (`None` for every other mode): the
    /// `project.scan` reply's `project_root`, which the preview path
    /// needs to strip absolute file paths down to `files:` node ids.
    /// Rides the slot so a restored Modules tree keeps the root it was
    /// scanned against instead of whichever workspace's scan last
    /// touched the shared field.
    scan_project_root: Option<String>,
    /// Provenance of the CURRENT contents: `false` = user state (stashed by
    /// `swap_active_tree` — cursor/expansion the user built; a reply
    /// rebuild must never destroy it), `true` = filled by a PARKED reply.
    /// A later parked reply MAY replace reply-provenance contents — replies
    /// are server-ordered, so newest wins (codex r5: two `.` toggles then a
    /// mode switch — reply #1 filled the empty slot, reply #2 with the
    /// FINAL visibility was dropped by the plain empty-only rule). NOT true
    /// for `project.scan`: `kernel.request` runs off-loop, so two scans for
    /// the same key can complete out of order — that consumer gates on its
    /// own per-key generation (`next_project_scan_gen`) before ever
    /// reaching this field, rather than trusting arrival order.
    from_reply: bool,
}

/// Parking lot for every nav tree that is NOT the active `(mode, scope)`.
/// The active tree lives on `State.tree`; the store's invariant is that it
/// never holds the active key (swap takes the entering slot OUT).
pub(in crate::ui) struct TreeStore {
    slots: std::collections::HashMap<TreeKey, TreeSlot>,
}

impl TreeStore {
    pub(in crate::ui) fn new() -> Self {
        Self {
            slots: std::collections::HashMap::new(),
        }
    }

    /// Park a tree under its key (mode/workspace switch-away).
    fn stash(&mut self, key: TreeKey, slot: TreeSlot) {
        self.slots.insert(key, slot);
    }

    /// Remove and return the tree for `key` (switch-to). Take, not get:
    /// the active tree must never ALSO live in the store.
    fn take(&mut self, key: &TreeKey) -> Option<TreeSlot> {
        self.slots.remove(key)
    }

    /// Mutable access to a NON-active slot, for installing a reply that
    /// arrived for a key we're not currently viewing. Entry-or-default so
    /// a first reply for a never-visited key still lands instead of being
    /// dropped — the store is exactly where such a reply belongs.
    fn slot_mut(&mut self, key: TreeKey) -> &mut TreeSlot {
        self.slots.entry(key).or_default()
    }

    /// Drop every per-workspace slot for a destroyed workspace (Files +
    /// Modules — the two Workspace-scoped modes). Without this, destroy →
    /// recreate under the same slug would resurrect the OLD project's
    /// parked rows, and the non-empty slot would suppress the fresh
    /// tree.root the empty-slot loader gate otherwise fires. Global slots
    /// (Sessions/Hosts) are machine-level and survive by design.
    fn purge_workspace(&mut self, ws_key: &WsKey) {
        for mode in [Mode::Files, Mode::Modules] {
            self.slots
                .remove(&(mode, TreeScope::Workspace(ws_key.clone())));
        }
    }

    /// Whether a reply for `key` parks: into an empty (or absent) slot or over an earlier parked reply, never over
    /// user state (a stashed view).
    fn parks_reply(&self, key: &TreeKey) -> bool {
        self.slots.get(key).is_none_or(|slot| slot.view.rows().is_empty() || slot.from_reply)
    }

    /// Park a reply's tree under its key when `parks_reply` allows. Returns whether it parked.
    fn park_reply(&mut self, key: TreeKey, fill: impl FnOnce(&mut TreeView)) -> bool {
        let slot = self.slot_mut(key);
        if slot.view.rows().is_empty() || slot.from_reply {
            fill(&mut slot.view);
            slot.from_reply = true;
            true
        } else {
            false
        }
    }
}

/// The navigation pane's trees: the active tree with its mode, scroll and scan root, and the store parking every
/// other (mode, scope)'s tree. The store never holds the active key; every mode or row change swaps through `swap`.
pub(in crate::ui) struct Navigation {
    /// The active (mode, row)'s tree, the one the pane shows.
    tree: TreeView,
    /// Which root tree the pane is showing.
    mode: Mode,
    /// Persistent nav-pane scroll offset (vim-style scrolloff, updated each frame from the cursor's motion).
    scroll: u16,
    /// The active Modules scan's `project_root`, stripped from each Modules row's absolute file path to make the
    /// `files:` node id `preview.get` expects.
    scan_project_root: Option<String>,
    store: TreeStore,
}

impl Navigation {
    pub(in crate::ui) fn new(mode: Mode) -> Self {
        Self { tree: TreeView::new(), mode, scroll: 0, scan_project_root: None, store: TreeStore::new() }
    }

    pub(in crate::ui) fn tree(&self) -> &TreeView { &self.tree }
    pub(in crate::ui) fn mode(&self) -> Mode { self.mode }
    pub(in crate::ui) fn scroll(&self) -> u16 { self.scroll }
    pub(in crate::ui) fn scan_project_root(&self) -> Option<&str> { self.scan_project_root.as_deref() }

    /// The key the active tree belongs to, for the active row `ws_key`.
    pub(in crate::ui) fn active_key(&self, ws_key: &WsKey) -> TreeKey { (self.mode, mode_scope(self.mode, ws_key)) }

    /// Change the mode without swapping: a row's restore or first visit, whose swap follows once the mode is settled.
    pub(in crate::ui) fn set_mode(&mut self, mode: Mode) { self.mode = mode; }

    /// The frame's scroll after keeping the cursor in view.
    pub(in crate::ui) fn set_scroll(&mut self, scroll: u16) { self.scroll = scroll; }

    pub(in crate::ui) fn select(&mut self, idx: usize) { self.tree.select(idx); }
    pub(in crate::ui) fn select_clamped(&mut self, n: usize) { self.tree.select_clamped(n); }
    pub(in crate::ui) fn move_up(&mut self) { self.tree.move_up(); }
    pub(in crate::ui) fn move_down(&mut self) { self.tree.move_down(); }
    pub(in crate::ui) fn collapse_selected(&mut self) -> bool { self.tree.collapse_selected() }
    pub(in crate::ui) fn mark_selected_expanded(&mut self) { self.tree.mark_selected_expanded(); }
    pub(in crate::ui) fn mark_expanded(&mut self, id: &str) { self.tree.mark_expanded(id); }
    pub(in crate::ui) fn set_root(&mut self, root: TreeNode, children: Vec<TreeNode>) { self.tree.set_root(root, children); }
    pub(in crate::ui) fn apply_children(&mut self, parent_id: &str, children: Vec<TreeNode>) {
        self.tree.apply_children(parent_id, children);
    }

    /// The active Modules scan: its rows and the root they were scanned against.
    pub(in crate::ui) fn install_scan(&mut self, rows: Vec<TreeRow>, project_root: Option<String>) {
        self.scan_project_root = project_root;
        self.tree.set_flat(rows);
    }

    /// The active Sessions tree rebuilt, with every open host's rows re-spliced from the fresh listings.
    pub(in crate::ui) fn install_sessions(&mut self, root: TreeNode, children: Vec<TreeNode>, directory: &RowDirectory) {
        self.tree.set_root(root, children);
        super::sessions_tree::resplice_expanded_session_hosts(&mut self.tree, directory);
    }

    /// The Hosts root's local re-expansion, declined off the root or outside Hosts mode.
    pub(in crate::ui) fn expand_hosts_root(&mut self, children: Vec<TreeNode>) -> bool {
        super::hosts_tree::expand_hosts_root(&mut self.tree, self.mode, children)
    }

    /// The visible rows show a visibility the backend no longer serves: empty the active view (and its scroll), so
    /// the incoming root installs whichever view it reaches.
    pub(in crate::ui) fn clear_active(&mut self) {
        self.tree = TreeView::new();
        self.scroll = 0;
    }

    /// Park the departing view under `old_key` (user state, with its scroll and, for Modules, its scan root) and
    /// bring in `new_key`'s parked slot, or an empty view for a first visit. A Files view coming in shows `label`
    /// on its root row.
    pub(in crate::ui) fn swap(&mut self, old_key: TreeKey, new_key: TreeKey, label: Option<String>) {
        if old_key == new_key {
            return;
        }
        let old_view = std::mem::take(&mut self.tree);
        let old_scan_root = if matches!(old_key.0, Mode::Modules) {
            self.scan_project_root.clone()
        } else {
            None
        };
        self.store.stash(
            old_key,
            TreeSlot {
                view: old_view,
                scroll: self.scroll,
                scan_project_root: old_scan_root,
                from_reply: false, // user state — parked replies must not destroy it
            },
        );
        self.scroll = 0;
        if let Some(slot) = self.store.take(&new_key) {
            self.tree = slot.view;
            self.scroll = slot.scroll;
            if matches!(new_key.0, Mode::Modules) {
                if let Some(root) = slot.scan_project_root {
                    self.scan_project_root = Some(root);
                }
            }
        }
        // else: the tree is the empty TreeView mem::take left — a first
        // visit; the caller decides whether to fire the mode's loader.
        if matches!(new_key.0, Mode::Files) {
            if let Some(label) = label {
                self.tree.relabel_root("files:", label);
            }
        }
    }

    /// Whether a reply for a key not on screen parks (`TreeStore::parks_reply`).
    pub(in crate::ui) fn parks_reply(&self, key: &TreeKey) -> bool { self.store.parks_reply(key) }

    /// A Files root for a key not on screen: parked unless the slot holds user state.
    pub(in crate::ui) fn park_root(&mut self, key: TreeKey, root: TreeNode, children: Vec<TreeNode>) {
        self.store.park_reply(key, |view| view.set_root(root, children));
    }

    /// A children splice for a key not on screen goes into that key's slot.
    pub(in crate::ui) fn park_children(&mut self, key: TreeKey, parent_id: &str, children: Vec<TreeNode>) {
        self.store.slot_mut(key).view.apply_children(parent_id, children);
    }

    /// A Modules scan for a key not on screen: its slot takes the rows and the root they were scanned against.
    pub(in crate::ui) fn park_scan(&mut self, key: TreeKey, rows: Vec<TreeRow>, project_root: Option<String>) {
        let slot = self.store.slot_mut(key);
        slot.view.set_flat(rows);
        slot.scan_project_root = project_root;
        slot.from_reply = true;
    }

    /// A Sessions rebuild while another mode is up: parked like a root, open hosts re-spliced.
    pub(in crate::ui) fn park_sessions(
        &mut self, key: TreeKey, root: TreeNode, children: Vec<TreeNode>, directory: &RowDirectory,
    ) -> bool {
        self.store.park_reply(key, |view| {
            view.set_root(root, children);
            super::sessions_tree::resplice_expanded_session_hosts(view, directory);
        })
    }

    /// The parked tree for `key`, if one is parked.
    pub(in crate::ui) fn parked(&self, key: &TreeKey) -> Option<&TreeView> {
        self.store.slots.get(key).map(|slot| &slot.view)
    }

    /// A destroyed row's Files and Modules slots go, so a same-slug successor starts clean.
    pub(in crate::ui) fn purge_workspace(&mut self, ws_key: &WsKey) { self.store.purge_workspace(ws_key); }

    /// Move the default row's raw-slug slots to its `<default>` key; a slot already there wins.
    pub(in crate::ui) fn migrate_workspace(&mut self, from: &WsKey, to: &WsKey) -> Vec<Mode> {
        let mut dropped = Vec::new();
        for mode in [Mode::Files, Mode::Modules] {
            let from_key = (mode, TreeScope::Workspace(from.clone()));
            if let Some(slot) = self.store.take(&from_key) {
                let to_key = (mode, TreeScope::Workspace(to.clone()));
                match self.store.take(&to_key) {
                    None => self.store.stash(to_key, slot),
                    Some(existing) => {
                        self.store.stash(to_key, existing);
                        dropped.push(mode);
                    }
                }
            }
        }
        dropped
    }
}

impl State {
    /// The key the active tree currently belongs to. Computed, never stored —
    /// so it can't drift from `(mode, active host, active workspace)`.
    pub(in crate::ui) fn active_tree_key(&self) -> TreeKey {
        self.nav.active_key(&self.active_ws_key())
    }

    /// The single stash/load seam for the active tree. Every mode or
    /// workspace change routes through here: park the departing view under
    /// `old_key`, bring in `new_key`'s stored slot (or an empty view for a
    /// first visit). Callers compute `old_key` BEFORE changing the mode or
    /// the selected row and `new_key` after.
    ///
    /// The Files root-label reconcile lives here (moved from
    /// `restore_workspace_ui`): a loaded Files tree deliberately skips the
    /// re-fetch, so a root row captured under a stale label is patched to
    /// the active workspace's label on the way in.
    pub(in crate::ui) fn swap_active_tree(&mut self, old_key: TreeKey, new_key: TreeKey) {
        let label = self.active_workspace_label();
        self.nav.swap(old_key, new_key, label);
    }

    /// Force Files mode through the store seam. The badge/pending-nav
    /// consume paths flip the mode as a side-effect of driving a file
    /// preview; the flip must stash/load like any other mode change or the
    /// Files view would show the departing mode's rows (the old
    /// restore-validity hole).
    pub(in crate::ui) fn force_files_mode(&mut self) {
        if matches!(self.nav.mode(), Mode::Files) {
            return;
        }
        let old_key = self.active_tree_key();
        self.nav.set_mode(Mode::Files);
        let new_key = self.active_tree_key();
        self.swap_active_tree(old_key, new_key);
    }

    /// Switch the nav mode and fire that mode's data fetch. Shared by the
    /// f/m/s/h keybinds and the ADR-0019 `mode` command; no-op if already in
    /// `mode`.
    pub(in crate::ui) fn enter_mode(&mut self, mode: Mode) {
        if self.nav.mode() == mode {
            return;
        }
        tracing::info!(from = ?self.nav.mode(), to = ?mode, "enter_mode (tree swap + refresh follows)");
        // Swap the active tree through the store: the departing mode's tree
        // (cursor, expansion, scroll) parks under its key and the entering
        // mode's parked tree — if any — comes back, which is what makes
        // cursor position per-mode-persistent across switches. The loader
        // below fires ONLY for an empty (never-loaded) view — the same
        // empty-slot gate switch_to_workspace uses. An unconditional refetch
        // here would defeat the persistence it restores: set_root/set_flat
        // rebuild root+level-1, so a cursor on a NESTED file (not in the
        // fresh level-1 rows) falls to 0 and every expansion collapses
        // (codex r3). Staleness of a restored view is the accepted residual
        // (parked-slot refresh policy, ops TODO) — identical to the
        // workspace-switch restore semantics.
        let old_key = self.active_tree_key();
        self.nav.set_mode(mode);
        let new_key = self.active_tree_key();
        self.swap_active_tree(old_key, new_key);
        // A mode change invalidates a one-shot Files reveal armed by
        // drive_same_ws_open / a workspace switch (the user navigated away from
        // the file they were being shown). Symmetric with the
        // switch_to_workspace clear. (The old cross-mode edge — a `files`
        // tree.root admitted while in Modules mode — is gone: installs route
        // by key now, so a Files reply can't touch a Modules view at all.)
        self.pending_switch_reveal = None;
        match mode {
            // Files/Modules: fire the loader ONLY for an empty (never-loaded)
            // view — a restored view keeps its nested cursor + expansion
            // (the refetch's set_root/set_flat would collapse them; codex
            // r3). Content staleness is the accepted parked-slot residual.
            Mode::Files => {
                if self.nav.tree().rows().is_empty() {
                    if let Err(e) = self.send(OutgoingReq::TreeRoot {
                        mode: "files".to_string(),
                        workspace_id: self.selected.workspace_id().clone(),
                    }) {
                        tracing::warn!(error = %e, "drop tree.root request — channel closed");
                    }
                } else {
                    self.refresh_restored_files_tree();
                }
            }
            Mode::Modules => {
                if self.nav.tree().rows().is_empty() {
                    let generation = self
                        .next_project_scan_gen(self.selected.host().clone(), self.selected.workspace_id().clone());
                    if let Err(e) = self.send(OutgoingReq::ProjectScan {
                        workspace_id: self.selected.workspace_id().clone(),
                        generation,
                    }) {
                        tracing::warn!(error = %e, "drop project.scan request — channel closed");
                    }
                }
            }
            // Sessions/Hosts: ALWAYS refresh — these are live status lists
            // where freshness beats restore, their rows are level-1 so
            // set_root's node-id re-anchor keeps the cursor losslessly, and
            // Sessions' parked slot deliberately goes stale between visits
            // (populated parks drop refreshes under the empty-only rule).
            Mode::Sessions => {
                // ADR 0042 L2a codex review, item A: Sessions mode's tree
                // spans EVERY connected host (host-grouped), so entering
                // it is exactly the "explicit global refresh" case — fan
                // out to every connection, not just active_host, or a
                // non-active host's rows go stale/empty while the user is
                // looking straight at them.
                for (host, _) in &self.conns {
                    if let Err(e) = self.send_to(host, OutgoingReq::WorkspaceList) {
                        tracing::warn!(error = %e, %host, "drop workspace.list request — channel closed");
                    }
                }
            }
            Mode::Hosts => {
                self.populate_hosts_tree();
                self.select_active_host();
            }
        }
        self.persist_resume_state();
    }
}

#[cfg(test)]
#[path = "tree_store_tests.rs"]
mod tests;
