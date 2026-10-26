//! Where the navigation cursor lands and when its follow fires: the settle debounce, the startup, relaunch and
//! reconnect placements, the README default, the pin, and the per-row project-scan clock.

use super::Mode;
use crate::net::dial::HostKey;
use crate::ui::WsKey;

#[derive(Default)]
pub(in crate::ui) struct NavCursor {
    /// (mode, cursor row) the last frame saw; a change is a cursor move.
    last_cursor_pos: Option<(Mode, usize)>,
    /// When the cursor last moved; while `NAV_FIRE_DEBOUNCE` has not passed, the cursor follow holds its requests.
    cursor_moved_at: Option<std::time::Instant>,
    /// `--start-selected <n>`, consumed by the first tree that lands for the startup mode.
    pending_initial_selection: Option<usize>,
    /// The relaunch-persisted `(selected node id, scroll)`, applied best-effort to the first matching Files root.
    pending_resume_nav: Option<(String, u16)>,
    /// Rows whose first Files root already placed the cursor (on the README, or by a resume), so a refresh never
    /// moves it again.
    nav_readme_defaulted: std::collections::HashSet<WsKey>,
    /// A reconnect's `(workspace, cursor node)`: the next Files root for that workspace re-reveals the node.
    restore_nav_after_resume: Option<(Option<String>, String)>,
    /// C2 pin-and-leave: the node the preview stays on while the cursor roams.
    pinned_preview_node_id: Option<String>,
    /// `project.scan` generations per (host, workspace): scans for different rows are valid in flight together.
    project_scan_req_gen: std::collections::HashMap<(HostKey, Option<String>), u64>,
}

impl NavCursor {
    pub(in crate::ui) fn new(initial_selection: Option<usize>, resume_nav: Option<(String, u16)>) -> Self {
        Self { pending_initial_selection: initial_selection, pending_resume_nav: resume_nav, ..Self::default() }
    }

    /// The frame's cursor position: whether it has sat still for `debounce` since its last move. A move restarts
    /// the wait; a settled cursor forgets its move, so the follow fires every settled frame.
    pub(in crate::ui) fn settled(&mut self, pos: (Mode, usize), debounce: std::time::Duration) -> bool {
        if self.last_cursor_pos != Some(pos) {
            self.last_cursor_pos = Some(pos);
            self.cursor_moved_at = Some(std::time::Instant::now());
        }
        let debouncing = self.cursor_moved_at.map(|t| t.elapsed() < debounce).unwrap_or(false);
        if !debouncing {
            self.cursor_moved_at = None;
        }
        !debouncing
    }

    /// Whether the startup selection has been consumed (the harness one-shots wait for it).
    pub(in crate::ui) fn initial_selection_done(&self) -> bool { self.pending_initial_selection.is_none() }
    pub(in crate::ui) fn take_initial_selection(&mut self) -> Option<usize> { self.pending_initial_selection.take() }
    pub(in crate::ui) fn take_resume_nav(&mut self) -> Option<(String, u16)> { self.pending_resume_nav.take() }

    /// The row's first Files root: true once, after which the row keeps whatever cursor it has.
    pub(in crate::ui) fn first_root_for(&mut self, row: WsKey) -> bool { self.nav_readme_defaulted.insert(row) }

    /// The default row's raw-slug marker moves to its `<default>` key.
    pub(in crate::ui) fn migrate_root_marker(&mut self, from: &WsKey, to: WsKey) {
        if self.nav_readme_defaulted.remove(from) {
            self.nav_readme_defaulted.insert(to);
        }
    }

    pub(in crate::ui) fn arm_resume_restore(&mut self, workspace_id: Option<String>, node_id: String) {
        self.restore_nav_after_resume = Some((workspace_id, node_id));
    }

    pub(in crate::ui) fn take_resume_restore(&mut self) -> Option<(Option<String>, String)> {
        self.restore_nav_after_resume.take()
    }

    pub(in crate::ui) fn pinned(&self) -> &Option<String> { &self.pinned_preview_node_id }
    pub(in crate::ui) fn pin(&mut self, node_id: String) { self.pinned_preview_node_id = Some(node_id); }
    pub(in crate::ui) fn take_pin(&mut self) -> Option<String> { self.pinned_preview_node_id.take() }

    /// A row's restore brings back its own pin; a first visit has none.
    pub(in crate::ui) fn restore_pin(&mut self, node_id: Option<String>) { self.pinned_preview_node_id = node_id; }

    pub(in crate::ui) fn issue_scan(&mut self, host: HostKey, workspace_id: Option<String>) -> u64 {
        let gen = self.project_scan_req_gen.entry((host, workspace_id)).or_insert(0);
        *gen += 1;
        *gen
    }

    /// The latest scan generation issued for the row, 0 before any.
    pub(in crate::ui) fn latest_scan(&self, host: &HostKey, workspace_id: &Option<String>) -> u64 {
        self.project_scan_req_gen.get(&(host.clone(), workspace_id.clone())).copied().unwrap_or(0)
    }
}
