//! Listing declarations and State coordination of directory/presentation effects.

use super::*;

/// Project one host's `WorkspaceInfo` rows into what `fe.sessions`
/// declares (session-listing brief decision 2) — a free function, no
/// `State` dependency, so the projection is directly unit-testable. Only
/// rows with a non-empty `agent_handle` are declared: a row that never
/// joined names nobody (ADR 0046), and inventing a name for it is the
/// confident-but-wrong failure this feature exists to close. The other
/// three fields are copied through verbatim — no derived state, so the
/// hub can never be MORE wrong than the strip the person on this box
/// sees.
pub(in crate::ui) fn declared_sessions_from(
    workspaces: &[crate::net::transport::WorkspaceInfo],
) -> Vec<sot_protocol::DeclaredSession> {
    workspaces
        .iter()
        .filter(|w| !w.agent_handle.is_empty())
        .map(|w| sot_protocol::DeclaredSession {
            handle: w.agent_handle.clone(),
            state: w.agent_state.clone(),
            summary: w.agent_summary.clone(),
            status_at: w.agent_status_at.clone(),
        })
        .collect()
}

/// The connection line for the `selected` computer: `connected · <host>:<row> · rev N` only while its link is up (with the
/// hello's `host` label, once one has landed); while it is down, the words its last disconnect wrote
/// (`disconnected · <reason>`), never `connected`; before its first answer (its first dial neither up nor failed),
/// nothing, so the line is left as it is.
pub(in crate::ui) fn connection_line(
    selected: &HostKey, up: &std::collections::HashMap<HostKey, bool>, last_disconnect: &std::collections::HashMap<HostKey, String>,
    host: Option<&str>, row: &str, revision: u64,
) -> Option<String> {
    match up.get(selected) {
        Some(true) => host.map(|host| format!("connected · {host}:{row} · rev {revision}")),
        Some(false) => last_disconnect.get(selected).map(|reason| super::replies::disconnect_status(reason, None)),
        None => None,
    }
}

impl State {
    /// Reconcile owed results and chat drafts against the directory's prepared retirement report, before commitment.
    /// Only currently listed rows can acquire a pending result; a disconnect and a kept default do not retire one. A
    /// retired row's chat draft is dropped with its results.
    pub(in crate::ui) fn reconcile_pending_results(&mut self, update: &RowDirectoryUpdate) {
        let effects = self.result_ledger.reconcile_retired(update.retired(), self.selected.host());
        self.apply_result_effects(effects);
        self.chat.retire(update.retired());
    }

    /// Badge changes reorder the existing projection without a directory refresh.
    pub(in crate::ui) fn resort_strip(&mut self) {
        let badged = self.badged_keys();
        self.row_presentation.resort(self.selected.host(), self.selected.workspace_id().as_deref(), badged);
    }

    /// Take the strip's next order, sort by it at once with the current badges, and keep the choice.
    pub(in crate::ui) fn cycle_strip_order(&mut self) {
        let order = self.row_presentation.order().next();
        let badged = self.badged_keys();
        self.row_presentation.set_order(order, badged);
        self.persist_resume_state();
        self.status.set(format!("Session order: {}", order.name()));
    }

    /// Apply listing effects in order: badges/presentation, REPL catch-up, default migration, status.
    /// The owner pins using the fresh default, preserves missing-row history, and exports migration
    /// on every applicable refresh: a late reply can have created another raw-slug entry.
    pub(in crate::ui) fn rebuild_workspace_caches(&mut self) {
        self.refresh_badge_index();
        let hosts = self.ordered_hosts();
        let badged = self.badged_keys();
        let effects = self.row_presentation.refresh(
            &self.row_directory, &hosts, self.selected.host(),
            self.selected.workspace_id().as_deref(), badged, std::time::Instant::now,
        );
        // Insert only: empty old-daemon values must not erase frame-driven lifecycle entries.
        self.repl_model.apply_listing_catchup(effects.repl_catchup);
        if let Some(target) = effects.default_migration {
            self.migrate_default_slug_keys(target);
        }
        self.rebuild_connection_status();
    }

    /// Rebuild the chrome status line to reflect the currently active
    /// workspace. Format: `connected · <host>:<workspace_label> · rev N`.
    /// Falls back to the slug if presentation labels haven't been populated
    /// yet, and to the daemon's project_root basename for the default
    /// workspace. No-op until the hello response has landed (no host). A
    /// state line: it never replaces words still held on the line.
    pub(in crate::ui) fn rebuild_connection_status(&mut self) {
        let label = self
            .active_workspace_label()
            .unwrap_or_else(|| "default".to_string());
        let line = connection_line(
            self.selected.host(), &self.hosts.host_connected, &self.hosts.last_disconnect,
            self.active_link.host_label(), &label, self.active_link.revision(),
        );
        if let Some(line) = line {
            self.status.show_state(line);
        }
    }

    /// The end of a hold: once one has passed at `now`, the connection line is rebuilt, and true says to redraw.
    pub(in crate::ui) fn expire_status_hold(&mut self, now: std::time::Instant) -> bool {
        if !self.status.expire_hold(now) {
            return false;
        }
        self.rebuild_connection_status();
        true
    }

    /// Display label for the active workspace: the per-workspace name from
    /// `workspace.list` (falling back to the slug), or the daemon's
    /// project_root basename for the default workspace. This is also the
    /// basename of the Files-mode root directory, so it doubles as the
    /// expected nav root-row label (used to reconcile a stale root after a
    /// snapshot restore). A default selection has no label before hello.
    pub(in crate::ui) fn active_workspace_label(&self) -> Option<String> {
        self.row_presentation.active_label(
            self.selected.host(),
            self.selected.workspace_id().as_deref(),
            self.active_link.root_basename(),
        )
    }
}

#[cfg(test)]
#[path = "workspace_list_tests.rs"]
mod tests;
