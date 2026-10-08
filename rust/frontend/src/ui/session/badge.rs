//! The badge floor: a result for a row the window is not on marks the row and never switches the view.

use super::*;
use crate::net::transport::ResultAttemptId;

/// Status-line text for a badged (pending) nav.preview result (ADR 0025 §1).
/// Pure so the badge-floor entry point's user-facing string is unit-testable
/// without constructing a full `State`. Reads as "a result is ready for this
/// workspace; switch to it to view".
fn pending_nav_status(ws: &str, path: &str) -> String {
    format!("result ready · {ws} · {path} — switch to view")
}

/// One try at showing an owed result on its row, with the certificates it has earned.
#[derive(Clone, Debug, PartialEq)]
struct ResultAttempt {
    serial: u64,
    node_id: String,
    preview_gen: u64,
    cursor_landed: bool,
    preview_installed: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct PendingNav {
    path: String,
    result_serial: u64,
    attempt: Option<ResultAttempt>,
}

/// Results owed to current listed canonical rows; authoritative removal or identity replacement invalidates their entries and attempts.
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::ui) struct PendingResults {
    entries: HashMap<ResultRowIdentity, PendingNav>,
    /// The listed strip keys the entries join to; `State::refresh_badge_index` keeps it current.
    strip: std::collections::HashSet<WsKey>,
}

impl PendingResults {
    pub(in crate::ui) fn contains_key(&self, key: &WsKey) -> bool {
        self.strip.contains(key)
    }
}

impl State {
    /// Badge-floor entry point (ADR 0025 §1): a `nav.preview` result for workspace `ws` arrived while
    /// the FE views another row. The row's strip name is badged and the status line says where the
    /// result waits; the view is never switched. A newer result replaces the owed one for the row.
    pub(in crate::ui) fn mark_pending_nav(&mut self, host: HostKey, ws: String, path: String) {
        let target = match resolve_listed_workspace(&self.workspace_lists, &host, &ws) {
            Ok(target) => target,
            Err(reason) => return self.refuse_result(&reason),
        };
        let Some(serial) = self.result_serial.checked_add(1) else {
            return self.refuse_result("result serial space exhausted");
        };
        self.result_serial = serial;
        self.status = pending_nav_status(&ws, &path);
        let entry = PendingNav {
            path,
            result_serial: serial,
            attempt: None,
        };
        self.pending_nav
            .entries
            .insert(target.identity().clone(), entry);
        self.refresh_badge_index();
        self.resort_strip();
        self.window.request_redraw();
    }

    /// Strip rows carrying an owed result: the entries joined to the current listed rows.
    pub(in crate::ui) fn badged_keys(&self) -> std::collections::HashSet<WsKey> {
        let listed = |id: &ResultRowIdentity| {
            let rows = self.workspace_lists.get(&id.host)?;
            let row = rows
                .iter()
                .find(|row| row.workspace_id == id.workspace_id)?;
            Some((id.host.clone(), row.slug.clone()))
        };
        self.pending_nav.entries.keys().filter_map(listed).collect()
    }

    pub(in crate::ui) fn refresh_badge_index(&mut self) {
        self.pending_nav.strip = self.badged_keys();
    }

    /// The canonical ids of the rows on `host` that owe a result.
    pub(in crate::ui) fn pending_nav_workspace_ids(&self, host: &HostKey) -> Vec<String> {
        let ids = self
            .pending_nav
            .entries
            .keys()
            .filter(|id| &id.host == host);
        ids.map(|id| id.workspace_id.to_string()).collect()
    }

    /// Start a fresh attempt at showing the result owed to `target`, with its preview generation. The entry stays until a presentation acknowledges it.
    pub(in crate::ui) fn begin_result_attempt(
        &mut self,
        target: &ResolvedWorkspace,
    ) -> Option<(ResultAttemptId, String, u64)> {
        let serial = self.attempt_serial.checked_add(1).or_else(|| {
            self.refuse_result("result attempt serial space exhausted");
            None
        })?;
        let entry = self.pending_nav.entries.get_mut(target.identity())?;
        self.attempt_serial = serial;
        self.preview_req_gen += 1;
        let node_id = format!("files:{}", entry.path);
        entry.attempt = Some(ResultAttempt {
            serial,
            node_id: node_id.clone(),
            preview_gen: self.preview_req_gen,
            cursor_landed: false,
            preview_installed: false,
        });
        let id = ResultAttemptId {
            workspace_id: target.identity().workspace_id.to_string(),
            result_serial: entry.result_serial,
            attempt_serial: serial,
        };
        Some((id, node_id, self.preview_req_gen))
    }

    /// Drop an attempt whose first request could not be sent; the owed result stays.
    pub(in crate::ui) fn abandon_result_attempt(&mut self, attempt: &ResultAttemptId) {
        if let Some(entry) = self.attempt_entry_mut(&self.active_host.clone(), attempt) {
            entry.attempt = None;
        }
        self.result_reveal = None;
    }

    /// The entry whose current attempt is `attempt`, while its row is still listed on `host`.
    fn attempt_entry_mut(
        &mut self,
        host: &HostKey,
        attempt: &ResultAttemptId,
    ) -> Option<&mut PendingNav> {
        let target =
            resolve_listed_workspace(&self.workspace_lists, host, &attempt.workspace_id).ok()?;
        let entry = self.pending_nav.entries.get_mut(target.identity())?;
        let current = entry.attempt.as_ref()?;
        (target.identity().workspace_id == attempt.workspace_id
            && entry.result_serial == attempt.result_serial
            && current.serial == attempt.attempt_serial)
            .then_some(entry)
    }

    /// The attempt on the active row, with that row's identity.
    fn active_attempt(&mut self) -> Option<(ResultRowIdentity, &mut ResultAttempt)> {
        let id = self.active_result_workspace()?.identity().clone();
        let attempt = self.pending_nav.entries.get_mut(&id)?.attempt.as_mut()?;
        Some((id, attempt))
    }

    /// Whether `attempt` is the one the active view's reveal works for: its row still listed, its
    /// serials current and the attempt not abandoned by a switch.
    pub(in crate::ui) fn admits_result_attempt(
        &mut self,
        host: &HostKey,
        attempt: &ResultAttemptId,
    ) -> bool {
        *host == self.active_host
            && self.result_reveal.as_ref() == Some(attempt)
            && self.attempt_entry_mut(host, attempt).is_some()
    }

    /// The cursor reached the file the active attempt owes; the reveal is finished.
    pub(in crate::ui) fn result_cursor_landed(&mut self, target_id: &str) {
        let Some(attempt) = self.result_reveal.clone() else {
            return;
        };
        let host = self.active_host.clone();
        let landed = self
            .attempt_entry_mut(&host, &attempt)
            .and_then(|entry| entry.attempt.as_mut());
        if let Some(a) = landed.filter(|a| a.node_id == target_id) {
            a.cursor_landed = true;
            self.result_reveal = None;
        }
    }

    /// A preview reply that passed the generation gate was installed.
    pub(in crate::ui) fn result_preview_installed(
        &mut self,
        generation: u64,
        node_id: Option<&str>,
    ) {
        if let Some((_, a)) = self.active_attempt() {
            if a.preview_gen == generation && node_id == Some(a.node_id.as_str()) {
                a.preview_installed = true;
            }
        }
    }

    /// A frame was presented: acknowledge the active row's result once its attempt holds both
    /// certificates and the frame showed that file under the cursor.
    pub(in crate::ui) fn acknowledge_presented_result(&mut self) {
        let cursor_id = self
            .tree
            .rows
            .get(self.tree.selected)
            .map(|row| row.node.id.clone());
        let shown = self.preview_src_node_id.clone();
        let Some((id, a)) = self.active_attempt() else {
            return;
        };
        if a.cursor_landed
            && a.preview_installed
            && shown.as_deref() == Some(a.node_id.as_str())
            && cursor_id.as_deref() == Some(a.node_id.as_str())
        {
            self.pending_nav.entries.remove(&id);
            self.refresh_badge_index();
            self.resort_strip();
        }
    }

    /// The listed row is gone or replaced: its owed result and attempts die with it.
    pub(in crate::ui) fn invalidate_result_row(&mut self, host: &HostKey, workspace_id: &str) {
        self.pending_nav
            .entries
            .retain(|id, _| !(&id.host == host && id.workspace_id == workspace_id));
        let owned = self
            .result_reveal
            .as_ref()
            .is_some_and(|a| a.workspace_id == workspace_id);
        if owned && *host == self.active_host {
            self.result_reveal = None;
            self.pending_reveal = None;
            self.pending_switch_reveal = None;
            self.reveal_awaiting = None;
            self.reveal_refetched = None;
            self.driven_preview_hold_cursor = None;
        }
        self.refresh_badge_index();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl State {
        /// The path owed to the listed strip row `(host, slug)`, if any.
        pub(in crate::ui) fn pending_result_path(
            &self,
            host: &HostKey,
            slug: &str,
        ) -> Option<String> {
            let target = resolve_listed_workspace(&self.workspace_lists, host, slug).ok()?;
            self.pending_nav
                .entries
                .get(target.identity())
                .map(|entry| entry.path.clone())
        }
    }

    #[test]
    fn pending_nav_status_names_workspace_and_path() {
        // The badge floor's user-facing status string (ADR 0025 §1) must name
        // both the workspace and the waiting path so the user knows where the
        // result is, and read as a switch prompt (non-disruptive — we never
        // yanked the view).
        let s = pending_nav_status("mypackage", "src/edge.jl");
        assert!(s.contains("mypackage"), "status names the workspace");
        assert!(s.contains("src/edge.jl"), "status names the pending path");
        assert!(
            s.contains("switch"),
            "status reads as a switch-to-view prompt, not a forced nav"
        );
    }
}
