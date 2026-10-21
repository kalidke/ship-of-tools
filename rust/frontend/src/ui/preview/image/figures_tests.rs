use super::*;

// --- Bug 1: an early `figure.get` failure must reach a terminal state
// (not warn-and-drop), and a markdown reload must make a previously-
// failed figure URL retryable — but nothing SHORT of that reload (in
// particular, a cached-bytes reflow) may clear it, or the failure
// refires every frame forever (round-2 field report). ---

#[test]
fn fail_figure_clears_pending_and_marks_failed() {
    let mut pending: std::collections::HashSet<String> =
        ["figures/x.png".to_string()].into_iter().collect();
    let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
    fail_figure(&mut pending, &mut failed, "figures/x.png".to_string());
    assert!(!pending.contains("figures/x.png"));
    assert!(failed.contains("figures/x.png"));
}

#[test]
fn failed_figure_stays_skipped_across_repeated_dispatch_until_seam_clears_it() {
    // This is the storm regression: `dispatch_pending_figures` calls
    // `figure_already_handled` on every walk, including the
    // cached-bytes reflow passes (apply_text_scale, needs_md_reflow,
    // workspace-switch restore) that `render_preview_source` also
    // serves. None of those may un-skip a failed url — only clearing
    // `failed` (which only the fresh IncomingEvt::Preview seam does)
    // may. Simulating N reflow passes with `failed` untouched must
    // keep skipping; only an explicit clear (the seam) reopens it.
    let pending: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
    failed.insert("figures/dead.png".to_string());
    for _ in 0..5 {
        assert!(
            figure_already_handled(false, &pending, &failed, "figures/dead.png"),
            "a terminally-failed url must stay skipped across repeated reflow passes"
        );
    }
    failed.clear(); // the one thing a genuine markdown reload does
    assert!(!figure_already_handled(
        false,
        &pending,
        &failed,
        "figures/dead.png"
    ));
}
