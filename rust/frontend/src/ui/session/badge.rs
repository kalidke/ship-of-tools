//! The badge floor: a result for a row the window is not on marks the row and never switches the view.

use super::*;

/// Status-line text for a badged (pending) nav.preview result (ADR 0025 §1).
/// Pure so the badge-floor entry point's user-facing string is unit-testable
/// without constructing a full `State`. Reads as "a result is ready for this
/// workspace; switch to it to view".
fn pending_nav_status(ws: &str, path: &str) -> String {
    format!("result ready · {ws} · {path} — switch to view")
}

impl State {
    /// Badge-floor entry point (ADR 0025 §1). Records that a `nav.preview`
    /// result for workspace `ws` (workspace-relative `path`) arrived while the
    /// FE was viewing a *different* workspace, and surfaces it non-disruptively:
    /// the workspace's nav row + bottom-strip name badge "result pending", and
    /// the status line says where the result is waiting. The view is NEVER
    /// switched here — the user keeps their place; the pending preview is driven
    /// only when they later switch to `ws` (see `switch_to_workspace`). This is
    /// the floor's contract: a result always reaches the user, never silently
    /// dropped. Latest-wins per workspace. The future `op::FE_COMMAND` handler
    /// will reuse this method.
    pub(in crate::ui) fn mark_pending_nav(&mut self, host: HostKey, ws: String, path: String) {
        self.status = pending_nav_status(&ws, &path);
        self.pending_nav.insert((host, ws), path);
        self.resort_strip();
        self.window.request_redraw();
    }

    /// Rows carrying a pending badge-floor result (ADR 0025 §1) — the
    /// `activity_order` input that lives in FE state, not the registry.
    pub(in crate::ui) fn badged_keys(&self) -> std::collections::HashSet<WsKey> {
        self.pending_nav.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn pending_nav_insert_is_latest_wins_and_host_qualified() {
        // The pending_nav map is the badge-floor state mark_pending_nav
        // writes: keyed by WsKey (host, slug) -- ADR 0042 L2a codex review
        // item E, was a bare slug -- latest path wins per (host, slug),
        // and the SAME slug on two DIFFERENT hosts are separate entries
        // (the collision a bare-slug key used to let a non-active host's
        // nav.preview be mistaken for the active host's own badge). This
        // mirrors mark_pending_nav's `insert` without needing a full State.
        let mut pending_nav: HashMap<WsKey, String> = HashMap::new();
        let alpha_pkg: WsKey = ("alpha".to_string(), "mypackage".to_string());
        let alpha_other: WsKey = ("alpha".to_string(), "other".to_string());
        let beta_pkg: WsKey = ("beta".to_string(), "mypackage".to_string());
        pending_nav.insert(alpha_pkg.clone(), "src/a.jl".to_string());
        pending_nav.insert(alpha_other.clone(), "src/b.jl".to_string());
        pending_nav.insert(beta_pkg.clone(), "src/z.jl".to_string());
        // Latest-wins on the same (host, workspace).
        pending_nav.insert(alpha_pkg.clone(), "src/c.jl".to_string());
        assert_eq!(pending_nav.len(), 3, "one entry per (host, workspace)");
        assert_eq!(
            pending_nav.get(&alpha_pkg).map(String::as_str),
            Some("src/c.jl"),
            "latest result for a (host, workspace) supersedes the earlier one"
        );
        assert_eq!(
            pending_nav.get(&alpha_other).map(String::as_str),
            Some("src/b.jl")
        );
        // beta's "mypackage" is untouched by alpha's inserts, even though
        // the slug is identical.
        assert_eq!(
            pending_nav.get(&beta_pkg).map(String::as_str),
            Some("src/z.jl"),
            "a same-slug entry on a different host must not collide"
        );
    }
}
