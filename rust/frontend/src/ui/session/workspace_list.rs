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

impl State {
    /// Reconcile owed results against the directory's prepared retirement report, before commitment.
    /// Only currently listed rows can acquire a pending result; a disconnect and a kept default do not retire one.
    pub(in crate::ui) fn reconcile_pending_results(&mut self, update: &RowDirectoryUpdate) {
        let effects = self.result_ledger.reconcile_retired(update.retired(), &self.active_host);
        self.apply_result_effects(effects);
    }

    /// Badge changes reorder the existing projection without a directory refresh.
    pub(in crate::ui) fn resort_strip(&mut self) {
        let badged = self.badged_keys();
        self.row_presentation.resort(&self.active_host, self.active_workspace_id.as_deref(), badged);
    }

    /// Apply listing effects in order: badges/presentation, REPL catch-up, default migration, status.
    /// The owner pins using the fresh default, preserves missing-row history, and exports migration
    /// on every applicable refresh: a late reply can have created another raw-slug entry.
    pub(in crate::ui) fn rebuild_workspace_caches(&mut self) {
        self.refresh_badge_index();
        let hosts = self.ordered_hosts();
        let badged = self.badged_keys();
        let effects = self.row_presentation.refresh(
            &self.row_directory, &hosts, &self.active_host,
            self.active_workspace_id.as_deref(), badged, std::time::Instant::now,
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
    /// workspace. No-op until the hello response has landed (no host).
    pub(in crate::ui) fn rebuild_connection_status(&mut self) {
        // Don't clobber a fresh notify toast: hold it on the status line until
        // its sticky window elapses (a workspace switch would otherwise rebuild
        // over it immediately). Once elapsed, clear the flag and rebuild.
        if let Some(until) = self.notify_sticky_until {
            if std::time::Instant::now() < until {
                return;
            }
            self.notify_sticky_until = None;
        }
        let Some(host) = self.host.clone() else {
            return;
        };
        let label = self
            .active_workspace_label()
            .unwrap_or_else(|| "default".to_string());
        self.status = format!("connected · {host}:{label} · rev {}", self.last_revision);
    }

    /// Display label for the active workspace: the per-workspace name from
    /// `workspace.list` (falling back to the slug), or the daemon's
    /// project_root basename for the default workspace. This is also the
    /// basename of the Files-mode root directory, so it doubles as the
    /// expected nav root-row label (used to reconcile a stale root after a
    /// snapshot restore). A default selection has no label before hello.
    pub(in crate::ui) fn active_workspace_label(&self) -> Option<String> {
        self.row_presentation.active_label(
            &self.active_host,
            self.active_workspace_id.as_deref(),
            self.daemon_root_basename.as_deref(),
        )
    }
}

#[cfg(test)]
#[path = "workspace_list_tests.rs"]
mod tests;
