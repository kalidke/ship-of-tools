use super::*;

fn block(id: u64, rows: usize) -> ReplEntry {
    let mut e = entry(id, "code", false);
    if rows > 1 {
        e.frames.push(ReplFrame::Stdout { text: "row\n".repeat(rows - 1) });
    }
    e
}

fn pin_between(scroll: u16, before: &[ReplEntry], after: &[ReplEntry]) -> u16 {
    let row = ("host".into(), "row".into());
    let mut owner = ReplPresentation::<()>::default();
    owner.build(&row, before, false, 8.0, 16.0, scroll);
    owner.build(&row, after, false, 8.0, 16.0, scroll).1
}

#[test]
fn pinned_repl_scroll_lets_a_live_pane_follow_new_output() {
    assert_eq!(pin_between(0, &[block(5, 5), block(7, 3)], &[block(5, 5), block(7, 7)]), 0);
}

#[test]
fn pinned_repl_scroll_holds_the_view_still_when_the_tail_grows() {
    assert_eq!(pin_between(4, &[block(5, 5), block(7, 3)], &[block(5, 5), block(7, 5)]), 6);
}

#[test]
fn pinned_repl_scroll_counts_only_the_tail_when_the_cap_drops_an_entry() {
    assert_eq!(pin_between(6, &[block(1, 4), block(5, 5), block(7, 3)], &[block(5, 5), block(7, 4)]), 7);
}

#[test]
fn pinned_repl_scroll_follows_a_tail_that_shrank() {
    assert_eq!(pin_between(6, &[block(5, 5), block(7, 4)], &[block(5, 5), block(7, 3)]), 5);
}

#[test]
fn pinned_repl_scroll_leaves_the_view_alone_when_its_anchor_vanished() {
    assert_eq!(pin_between(6, &[block(5, 5), block(7, 4)], &[block(5, 5), block(9, 3)]), 6);
}

fn build(log: &[ReplEntry], starting: bool) -> Vec<RtLine<'static>> {
    let row = ("host".into(), "row".into());
    ReplPresentation::<()>::default().build(&row, log, starting, 8.0, 16.0, 0).0
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
    let lines = build(&log, true);
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
    let lines = build(&log, false);
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
    let lines = build(&[e], true);
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
    let lines = build(&[e], false);
    let text = rendered_text(&lines);
    assert!(
        text.contains("run()\nProgress: 10%\nProgress: 20%\nxyc\n\ndone"),
        "{text:?}"
    );
}
