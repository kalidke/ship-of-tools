use super::*;

/// Live means live: at the tail, new output follows, as a terminal does.
#[test]
fn pinned_repl_scroll_lets_a_live_pane_follow_new_output() {
    assert_eq!(pinned_repl_scroll(0, 7, 3, 12, &[(5, 0), (7, 5)]), 0);
}

/// Held back, the offset tracks the tail so the rows being read stay
/// under the arriving output instead of sliding away.
#[test]
fn pinned_repl_scroll_holds_the_view_still_when_the_tail_grows() {
    // Anchored entry 7 started at line 5 and spanned 3; it now spans 5.
    assert_eq!(pinned_repl_scroll(4, 7, 3, 10, &[(5, 0), (7, 5)]), 6);
}

/// The case a total-line delta gets wrong: at the 256-entry cap, an
/// arriving entry drops the oldest off the FRONT in the same frame. The
/// total can even fall while the tail grew, and compensating on the
/// total would slide the view once per eval. Measured at the tail, the
/// drop contributes nothing and only the real growth counts.
#[test]
fn pinned_repl_scroll_counts_only_the_tail_when_the_cap_drops_an_entry() {
    // Was [(1,0),(5,4),(7,9)] with 12 lines, anchor span 3. Entry 1 is
    // gone and the tail grew by exactly one line: total FELL 12 -> 9.
    assert_eq!(pinned_repl_scroll(6, 7, 3, 9, &[(5, 0), (7, 5)]), 7);
}

/// Entries shrink as well as grow — a finished entry with no measurable
/// elapsed time loses its "(running…)" line. A row removed between the
/// viewed rows and the tail shortens their distance from it, so the
/// offset drops with it.
#[test]
fn pinned_repl_scroll_follows_a_tail_that_shrank() {
    assert_eq!(pinned_repl_scroll(6, 7, 4, 8, &[(5, 0), (7, 5)]), 5);
}

/// The anchored entry is gone entirely: nothing trustworthy to measure
/// from, so leave the view alone rather than guess at a delta.
#[test]
fn pinned_repl_scroll_leaves_the_view_alone_when_its_anchor_vanished() {
    assert_eq!(pinned_repl_scroll(6, 7, 4, 8, &[(5, 0), (9, 5)]), 6);
}

fn entry(eval_id: u64, code: &str, in_flight: bool) -> ReplEntry {
    ReplEntry {
        eval_id,
        code: code.to_string(),
        frames: Vec::new(),
        elapsed_ms: 0,
        in_flight,
        pkg_mode: false,
        origin: None,
    }
}

fn rendered_text(lines: &[RtLine<'static>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn in_flight_entry_says_starting_while_repl_boots() {
    // THE repl_state deliverable: with the workspace's REPL child still
    // booting (precompiling), an in-flight entry must say so instead of
    // the generic "(running…)" — which is indistinguishable from a dead
    // kernel for the minutes a first-per-workspace boot takes.
    let log = vec![entry(1, "1+1", true)];
    let images = std::collections::HashMap::new();
    let (lines, _, _) = build_repl_lines(&log, &images, 800.0, 600.0, 8.0, 16.0, true);
    let text = rendered_text(&lines);
    assert!(
        text.contains("julia starting"),
        "starting boot must be named: {text}"
    );
    assert!(
        !text.contains("(running…)"),
        "the generic running line must be replaced, not doubled: {text}"
    );
}

#[test]
fn in_flight_entry_says_running_once_ready() {
    let log = vec![entry(1, "1+1", true)];
    let images = std::collections::HashMap::new();
    let (lines, _, _) = build_repl_lines(&log, &images, 800.0, 600.0, 8.0, 16.0, false);
    let text = rendered_text(&lines);
    assert!(text.contains("(running…)"), "{text}");
    assert!(!text.contains("julia starting"), "{text}");
}

#[test]
fn completed_entry_ignores_starting_flag() {
    // A finished entry renders its elapsed footer regardless of a boot
    // in progress (e.g. the user restarted the REPL after a run).
    let mut e = entry(1, "1+1", false);
    e.elapsed_ms = 42;
    let images = std::collections::HashMap::new();
    let (lines, _, _) = build_repl_lines(&[e], &images, 800.0, 600.0, 8.0, 16.0, true);
    let text = rendered_text(&lines);
    assert!(text.contains("(42 ms)"), "{text}");
    assert!(!text.contains("julia starting"), "{text}");
}

#[test]
fn a_carriage_return_in_output_overwrites_its_line() {
    // Progress output: each frame starts with `\r`, and one frame overwrites part of its line.
    let mut e = entry(1, "run()", false);
    e.frames = ["\rProgress: 10%", "\rProgress: 20%", "abc\rxy\n", "\n", "done\r\n"]
        .into_iter()
        .map(|t| ReplFrame::Stdout { text: t.to_string() })
        .collect();
    let (lines, _, _) = build_repl_lines(&[e], &std::collections::HashMap::new(), 800.0, 600.0, 8.0, 16.0, false);
    let text = rendered_text(&lines);
    assert!(
        text.contains("run()\nProgress: 10%\nProgress: 20%\nxyc\n\ndone"),
        "{text:?}"
    );
}
