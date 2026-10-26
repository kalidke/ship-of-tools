//! State coordinates the result ledger with navigation, presentation and transport.

use super::*;
use crate::net::transport::ResultAttemptId;

/// Status-line text for a badged (pending) nav.preview result (ADR 0025 §1).
/// Pure so the badge-floor entry point's user-facing string is unit-testable
/// without constructing a full `State`. Reads as "a result is ready for this
/// workspace; switch to it to view".
fn pending_nav_status(ws: &str, path: &str) -> String {
    format!("result ready · {ws} · {path} — switch to view")
}

impl State {
    pub(in crate::ui) fn mark_pending_nav(&mut self, host: HostKey, ws: String, path: String) {
        let status = pending_nav_status(&ws, &path);
        if let Err(reason) = self
            .result_ledger
            .record(&self.row_directory, &host, &ws, path)
        {
            return self.refuse_result(&reason);
        }
        self.status = status;
        self.refresh_badge_index();
        self.resort_strip();
        self.window.request_redraw();
    }

    pub(in crate::ui) fn badged_keys(&self) -> std::collections::HashSet<WsKey> {
        self.result_ledger.badged_keys(&self.row_directory)
    }

    pub(in crate::ui) fn refresh_badge_index(&mut self) {
        self.result_ledger.refresh_badges(&self.row_directory);
    }

    pub(in crate::ui) fn begin_result_attempt(
        &mut self,
        target: &ResolvedWorkspace,
    ) -> Option<AttemptStart> {
        let result = {
            let requests = &mut self.preview_requests;
            self.result_ledger.begin_attempt(target, || requests.issue_preview())
        };
        match result {
            Ok(Some(start)) => Some(start),
            Ok(None) => None,
            Err(reason) => {
                self.refuse_result(reason);
                None
            }
        }
    }

    pub(in crate::ui) fn abandon_result_attempt(&mut self, attempt: &ResultAttemptId) {
        self.result_ledger
            .abandon_attempt(&self.row_directory, self.selected.host(), attempt);
    }

    pub(in crate::ui) fn admits_result_attempt(
        &mut self,
        host: &HostKey,
        attempt: &ResultAttemptId,
    ) -> bool {
        self.result_ledger
            .admits_attempt(&self.row_directory, host, self.selected.host(), attempt)
    }

    pub(in crate::ui) fn result_cursor_landed(&mut self, node_id: &str) {
        self.result_ledger
            .cursor_landed(&self.row_directory, self.selected.host(), node_id);
    }

    pub(in crate::ui) fn result_preview_installed(
        &mut self,
        generation: u64,
        node_id: Option<&str>,
    ) {
        let target = self.active_result_workspace();
        self.result_ledger
            .preview_installed(target.as_ref(), generation, node_id);
    }

    pub(in crate::ui) fn acknowledge_presented_result(&mut self) {
        let target = self.active_result_workspace();
        let cursor = self
            .nav.tree()
            .rows()
            .get(self.nav.tree().selected())
            .map(|row| row.node.id.clone());
        let shown = self.preview_source.node_id().map(str::to_owned);
        let effects = self.result_ledger.acknowledge_presented(
            target.as_ref(),
            cursor.as_deref(),
            shown.as_deref(),
        );
        self.apply_result_effects(effects);
        if effects.refresh_badges {
            self.resort_strip();
        }
    }

    /// Apply navigation cleanup before committing a prepared directory replacement or bouncing rows.
    pub(in crate::ui) fn apply_result_effects(&mut self, effects: ResultEffects) {
        if effects.clear_reveal {
            self.pending_reveal = None;
            self.pending_switch_reveal = None;
            self.reveal_awaiting = None;
            self.reveal_refetched = None;
            self.preview_requests.abort_reveal();
        }
        if effects.refresh_badges {
            self.refresh_badge_index();
        }
    }

    #[cfg(test)]
    pub(in crate::ui) fn pending_result_path(
        &self,
        host: &HostKey,
        spelling: &str,
    ) -> Option<String> {
        let target = self.row_directory.resolve(host, spelling).ok()?;
        self.result_ledger
            .path(target.identity())
            .map(str::to_owned)
    }
}

#[cfg(test)]
#[path = "badge_tests.rs"]
mod tests;
