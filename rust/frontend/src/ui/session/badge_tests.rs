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
