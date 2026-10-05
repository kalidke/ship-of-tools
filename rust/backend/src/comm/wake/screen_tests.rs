//! Tests of the free-prompt screen reading: captured frames, menus, glyphs and the typed-line gate.

use super::*;

fn refused_on(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool) -> Option<&'static str> {
    match prompt_of(lines, cursor, agent, windows) {
        Ok(Prompt::Empty) => None,
        Ok(Prompt::HoldsLine) => Some("wake text left unsent"),
        Err(reason) => Some(reason),
    }
}

fn prompt_free_on(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool) -> bool {
    refused_on(lines, cursor, agent, windows).is_none()
}

fn lines(l: &[&str]) -> Vec<String> {
    l.iter().map(|s| s.to_string()).collect()
}

fn free(l: &[&str], cur: Option<(u16, u16)>) -> bool {
    prompt_free_on(&lines(l), cur, "claude", false)
}

const R: &str = "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}";

fn boxed(line: &str) -> Vec<String> {
    vec![rule80(), line.to_string(), rule80()]
}

#[test]
fn a_box_holding_the_wake_line_names_it() {
    let held = format!("\u{276f}\u{a0}{WAKE_LINE}");
    assert_eq!(prompt_of(&boxed(&held), Some((1, 2)), "claude", false), Ok(Prompt::HoldsLine));
    // As a wake leaves it: the cursor at the END of the text.
    let end = 2 + WAKE_LINE.chars().count() as u16;
    assert_eq!(prompt_of(&boxed(&held), Some((1, end)), "claude", false), Ok(Prompt::HoldsLine));
    assert_eq!(prompt_of(&boxed("\u{276f}\u{a0}"), Some((1, 2)), "claude", false), Ok(Prompt::Empty));
    assert_eq!(refused_on(&boxed("\u{276f}\u{a0}hello"), Some((1, 2)), "claude", false), Some("input not empty"));
}

fn bfree(line: &str, col: u16, windows: bool) -> bool {
    prompt_free_on(&boxed(line), Some((1, col)), "claude", windows)
}

#[test]
fn free_prompts() {
    for windows in [false, true] {
        // A suggestion as the wake reads it: the dim text blank.
        assert!(bfree("\u{276f}\u{a0}", 2, windows));
    }
}

#[test]
fn drafts_dialogs_and_missing_cursors_are_not_free() {
    for w in [false, true] {
        // The cursor at the draft's end (clause b), and text before the glyph (clause a).
        assert!(!bfree("\u{276f}\u{a0}hello", 7, w));
        assert!(!bfree("x\u{276f}\u{a0}", 3, w));
    }
    assert!(!free(&["\u{276f}", "Allow this action? (y/n)"], Some((1, 24))));
    assert!(!free(&["\u{276f}"], None));
    assert!(!free(&["\u{276f}"], Some((5, 1))));
}

#[test]
fn an_agent_without_a_predicate_is_never_free() {
    for windows in [false, true] {
        assert!(!prompt_free_on(&lines(&["\u{203a}"]), Some((0, 1)), "codex", windows));
        assert!(!prompt_free_on(&lines(&["\u{276f}"]), Some((0, 1)), "codex", windows));
    }
}

fn on(l: &[&str], cur: (u16, u16), windows: bool) -> bool {
    prompt_free_on(&lines(l), Some(cur), "claude", windows)
}

/// A claude row at rest, captured live on Linux (2026-10-01): cursor (8, 2), after a grey suggestion. Identifying text scrubbed.
const LINUX_IDLE: [&str; 13] = [
    "● Monitor event: a watched row changed state                                                                     │",
    "                                                                                                                 │",
    "● Nothing to act on; staying idle.                                                                               │",
    "  Waiting for the next case.                                                                                     │",
    "                                                                                                                 │",
    "✻ Sautéed for 3s · done 9:21 AM · 2 monitors still running                                                       │",
    "                                                                         ✔ Update installed · Restart to update  │",
    "───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
    "❯\u{a0}ready for the next case",
    "───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
    "  Opus 5.5 [00000000] acct ·think:xhigh | v2.1.285 | demo:main | 0 uncommitted",
    "  Session: 1k (in:1k out:1k) | $0.00",
    "  ⏵⏵ auto mode on · 2 monitors · ← for agents",
];

/// A claude row mid-turn, captured live on Linux (2026-10-01): the spinner is live above the box and the cursor still sits at (8, 2). Line 2 is a queued input echoed with the glyph OUTSIDE the box.
const LINUX_TURN_A: [&str; 13] = [
    "     (ctrl+b to run in background)                                                                               │",
    "                                                                                                                 │",
    "❯ [sot-comm] you have mail: run comm-poll.sh                                                                     │",
    "  ctrl+x ctrl+s to send now                                                                                      │",
    "                                                                                                                 │",
    "✢ Levitating… (56s · ↓ 3.4k tokens)                                                                              │",
    "                                                                         ✔ Update installed · Restart to update  │",
    "───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
    "❯\u{a0}Press up to edit queued messages",
    "───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
    "  Opus 5.5 [00000000] acct ·think:xhigh | v2.1.285 | demo:main | 0 uncommitted",
    "  Session: 1k (in:1k out:1k) | $0.00",
    "  ⏵⏵ auto mode on · 2 monitors · ← for agents",
];

// Windows: REAL captures of a claude row mid-turn on Windows (2026-10-01, Claude Code 2.1.282, the row's own
// screen read, trailing spaces trimmed by the reader), statusline id and cost scrubbed. rows 75, cols 203,
// cursor (69, 2) = row 7 here, col 2. The prompt is the single byte '>' and the rules are U+2500 x 203: Claude
// Code's `figures` fallback, chosen when its unicode check fails (on Windows: no WT_SESSION, TERM other than
// xterm-256color, ...); the statusline's '√' is the same fallback for '✔'.
const WIN_RULE: &str = "───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────";
/// Mid-turn: the spinner and the running tool are live above the box.
const WIN_TURN_A: [&str; 13] = [
    "  ⎿  Running… (8s)",
    "     (ctrl+b to run in background)",
    "",
    "✢ Prestidigitating… (38s · ↓ 584 tokens)",
    "  ⎿  Tip: Hit shift+tab to cycle between manual mode, auto-accept edit mode, and plan mode",
    "",
    WIN_RULE,
    ">",
    WIN_RULE,
    "  Opus 5.5 (1M context) [00000000] think:medium | v2.1.282 | demo:main | 0 uncommitted                                                                    √ Update installed · Restart to update",
    "  Session: 197k (in:197k out:471) | $0.00",
    "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
    "",
];
/// DERIVED from WIN_TURN_A (not captured): the turn ended; lines 0, 1 and 3 replaced by finished-turn text.
const WIN_REST: [&str; 13] = [
    "  ⎿  Done.",
    "",
    "",
    "✻ Worked for 38s",
    "  ⎿  Tip: Hit shift+tab to cycle between manual mode, auto-accept edit mode, and plan mode",
    "",
    WIN_RULE,
    ">",
    WIN_RULE,
    "  Opus 5.5 (1M context) [00000000] think:medium | v2.1.282 | demo:main | 0 uncommitted                                                                    √ Update installed · Restart to update",
    "  Session: 197k (in:197k out:471) | $0.00",
    "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
    "",
];
/// DERIVED from WIN_REST: a quoted ">" output line at row 1 (put the cursor on it at (1, 2)).
const WIN_QUOTED: [&str; 13] = [
    "  ⎿  Done.",
    "> quoted text",
    "",
    "✻ Worked for 38s",
    "  ⎿  Tip: Hit shift+tab to cycle between manual mode, auto-accept edit mode, and plan mode",
    "",
    WIN_RULE,
    ">",
    WIN_RULE,
    "  Opus 5.5 (1M context) [00000000] think:medium | v2.1.282 | demo:main | 0 uncommitted                                                                    √ Update installed · Restart to update",
    "  Session: 197k (in:197k out:471) | $0.00",
    "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
    "",
];

#[test]
fn linux_rest_capture_is_free() {
    // The grey suggestion as the wake reads it: blank.
    let mut idle = LINUX_IDLE;
    idle[8] = "\u{276f}\u{a0}";
    assert!(on(&idle, (8, 2), false));
}

/// Two live captures of a claude row at rest, scrubbed, one second apart: a background agent's footer below the box ticks (`3m 9s` to `3m 11s`); every other row is identical.
fn footer_frame(text: &str) -> Vec<String> {
    text.lines().filter(|l| !l.starts_with("cols=")).map(String::from).collect()
}

/// REAL: Claude Code 2.1.288, agent view off, one background sub-agent, 80x24 (2026-10-03), scrubbed. The screen
/// reader that took them trims U+00A0 (`str::trim_end`); the wake's reader keeps the prompt's, so with `nbsp` a row
/// that is the bare `❯` gets it back. The trailer gives the cursor.
fn cc288(text: &str, nbsp: bool) -> (Vec<String>, (u16, u16)) {
    let mut lines = Vec::new();
    let mut cursor = None;
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("cols=") {
            let at = rest.split_whitespace().find_map(|w| w.strip_prefix("cursor=")).expect("cursor= in the trailer");
            let (r, c) = at.split_once(',').expect("row,col");
            cursor = Some((r.parse().unwrap(), c.parse().unwrap()));
        } else if nbsp && l == "\u{276f}" {
            lines.push("\u{276f}\u{a0}".to_string());
        } else {
            lines.push(l.to_string());
        }
    }
    (lines, cursor.expect("a trailer"))
}

#[test]
fn cc288_agent_view_off_rest_reads_free() {
    let rests = [
        include_str!("../../../tests/fixtures/comm_wake/cc288-avoff-1-rest.txt"),
        include_str!("../../../tests/fixtures/comm_wake/cc288-avoff-4-esc.txt"),
        include_str!("../../../tests/fixtures/comm_wake/cc288-avoff-5-esc2.txt"),
    ];
    for text in rests {
        let (l, cur) = cc288(text, true);
        assert_eq!((l.len(), cur), (24, (15, 2)));
        for windows in [false, true] {
            assert_eq!(refused_on(&l, Some(cur), "claude", windows), None);
        }
    }
}

#[test]
fn cc288_rest_with_the_one_agent_footer_reads_free() {
    // DERIVED from the f6 rest capture, with the footer of the live M4 failure on rc9.12 (agent view on, 2.1.288).
    let (mut l, cur) = cc288(include_str!("../../../tests/fixtures/comm_wake/cc288-avoff-1-rest.txt"), true);
    l[19] = "  ⏵⏵ auto mode on (shift+tab to cycle) · /tasks to see subagents · ← 1 agent".to_string();
    for windows in [false, true] {
        assert_eq!(refused_on(&l, Some(cur), "claude", windows), None);
    }
}

#[test]
fn a_pane_too_narrow_for_the_wake_line_is_refused() {
    let b = |w: usize| vec!["\u{2500}".repeat(w), "\u{276f}\u{a0}".to_string(), "\u{2500}".repeat(w)];
    let need = 2 + WAKE_LINE.chars().count() + 2;
    for windows in [false, true] {
        assert_eq!(refused_on(&b(need - 1), Some((1, 2)), "claude", windows), Some("pane too narrow for the wake line"));
        assert_eq!(refused_on(&b(need), Some((1, 2)), "claude", windows), None);
    }
}

#[test]
fn the_agent_footer_is_not_watched() {
    let a = footer_frame(include_str!("../../../tests/fixtures/comm_wake/footer-frame-a.txt"));
    let b = footer_frame(include_str!("../../../tests/fixtures/comm_wake/footer-frame-b.txt"));
    assert_eq!((a.len(), b.len()), (75, 75));
    for windows in [false, true] {
        assert!(prompt_free_on(&a, Some((67, 2)), "claude", windows));
    }
    assert_ne!(a, b, "the whole-frame hold saw the footer tick");
    assert_eq!(held_rows(&a, 67), held_rows(&b, 67));
    assert_eq!(held_rows(&a, 67).len(), 69);
    assert!(is_rule(&held_rows(&a, 67)[68]));
}

#[test]
fn the_spinner_above_the_box_is_watched() {
    let mut linux = LINUX_TURN_A;
    linux[5] = "✢ Levitating… (57s · ↓ 3.4k tokens)";
    assert_ne!(held_rows(&lines(&linux), 8), held_rows(&lines(&LINUX_TURN_A), 8));
    let mut win = WIN_TURN_A;
    win[3] = "✢ Prestidigitating… (39s · ↓ 584 tokens)";
    assert_ne!(held_rows(&lines(&win), 7), held_rows(&lines(&WIN_TURN_A), 7));
}

#[test]
fn one_turn_frame_reads_free() {
    // One frame of a working row reads free (its box shows the glyph, NBSP and a queued-message hint); only
    // the hold in `wake_if_free` protects it (itest `a_working_row_is_not_typed_into_until_it_rests`). The
    // queued-message hint read blank if it is dim (unverified); either way the hold protects the row.
    let mut turn = LINUX_TURN_A;
    turn[8] = "\u{276f}\u{a0}";
    assert!(on(&turn, (8, 2), false));
    assert_eq!(on(&WIN_TURN_A, (7, 2), true), !nbsp_required(true));
    let mut nbsp = WIN_TURN_A;
    nbsp[7] = ">\u{a0}";
    assert!(on(&nbsp, (7, 2), true));
}

#[test]
fn windows_takes_both_glyphs_inside_the_box() {
    assert_eq!(on(&WIN_REST, (7, 2), true), !nbsp_required(true));
    let mut nbsp = WIN_REST;
    nbsp[7] = ">\u{a0}";
    assert!(on(&nbsp, (7, 2), true));
    let mut bare = WIN_REST;
    bare[7] = "\u{276f}";
    assert_eq!(on(&bare, (7, 2), true), !nbsp_required(true));
    bare[7] = "\u{276f}\u{a0}";
    assert!(on(&bare, (7, 2), true));
    let mut idle = LINUX_IDLE;
    idle[8] = "\u{276f}\u{a0}";
    assert!(on(&idle, (8, 2), true));
}

#[test]
fn windows_quoted_lines_are_not_free() {
    assert!(!on(&WIN_QUOTED, (1, 2), true));
    let mut quoted = WIN_QUOTED;
    quoted[1] = "\u{276f} quoted text";
    assert!(!on(&quoted, (1, 2), true));
}

/// SYNTHETIC, from the layout of Claude Code's permission dialog: every option is `[pointer-or-space, " ", label]`.
const PERM_MENU: [&str; 10] = [
    "Bash command",
    "",
    "  rm -rf build",
    "  Remove the build directory",
    "",
    "Do you want to proceed?",
    "",
    "\u{276f} 1. Yes",
    "  2. Yes, and don't ask again for rm commands",
    "  3. No, and tell Claude what to do differently (esc)",
];
/// SYNTHETIC, from the layout of the AskUserQuestion dialog.
const ASKQ_MENU: [&str; 6] = [
    "Which approach?",
    "",
    "\u{276f} 1. Fast path",
    "     Skip the cache",
    "  2. Safe path",
    "  3. Type something.",
];

fn with_rules<const N: usize>(menu: [&str; N], rows: [usize; 2]) -> [&str; N] {
    let mut m = menu;
    m[rows[0]] = R;
    m[rows[1]] = R;
    m
}

#[test]
fn menus_are_never_free() {
    let perm_boxed = with_rules(PERM_MENU, [6, 8]);
    let askq_boxed = with_rules(ASKQ_MENU, [1, 3]);
    for windows in [false, true] {
        assert!(!on(&PERM_MENU, (7, 2), windows));
        assert!(!on(&perm_boxed, (7, 2), windows));
        assert!(!on(&ASKQ_MENU, (2, 2), windows));
        assert!(!on(&askq_boxed, (2, 2), windows));
    }
    let mut perm_gt = PERM_MENU;
    perm_gt[7] = "> 1. Yes";
    let mut perm_gt_boxed = perm_boxed;
    perm_gt_boxed[7] = "> 1. Yes";
    let mut askq_gt = ASKQ_MENU;
    askq_gt[2] = "> 1. Fast path";
    let mut askq_gt_boxed = askq_boxed;
    askq_gt_boxed[2] = "> 1. Fast path";
    assert!(!on(&perm_gt, (7, 2), true));
    assert!(!on(&perm_gt_boxed, (7, 2), true));
    assert!(!on(&askq_gt, (2, 2), true));
    assert!(!on(&askq_gt_boxed, (2, 2), true));
    assert!(!free(&["\u{276f} 1. Yes"], Some((0, 2))));
}

#[test]
fn dialog_inputs_are_never_free() {
    for windows in [false, true] {
        let ask = "\u{276f} press 1-3 or type your answer";
        assert!(!prompt_free_on(&lines(&[ask]), Some((0, 2)), "claude", windows));
        assert!(!bfree(ask, 2, windows));
        assert!(!bfree("\u{276f} describe a task for a new session", 2, windows));
    }
}

#[test]
fn an_empty_idle_prompt_as_the_wake_reads_it_is_free() {
    let mut idle = LINUX_IDLE;
    idle[8] = "\u{276f}\u{a0}";
    for windows in [false, true] {
        assert!(on(&idle, (8, 2), windows));
        assert!(on(&idle, (8, 1), windows));
        assert!(bfree("\u{276f}\u{a0}", 2, windows));
        assert!(bfree("\u{276f}\u{a0}", 1, windows));
    }
}

#[test]
fn a_glyph_then_a_space_is_not_free() {
    for windows in [false, true] {
        assert!(!bfree("\u{276f} text", 2, windows));
    }
    assert!(!bfree("> text", 2, true));
}

#[test]
fn output_lines_are_not_free() {
    // The real queued-input echo of a working row: the glyph sits outside the box.
    for windows in [false, true] {
        assert!(!on(&LINUX_TURN_A, (2, 2), windows));
    }
}

#[test]
fn a_bare_glyph_follows_the_switch() {
    for windows in [false, true] {
        assert_eq!(bfree("\u{276f}", 1, windows), !nbsp_required(windows));
        assert_eq!(bfree("\u{276f}", 2, windows), !nbsp_required(windows));
        // The agents-view task box with an empty placeholder (a voice state) reads as a bare glyph: free
        // wherever the NBSP is not required, the named Windows-only gap.
        assert_eq!(bfree("\u{276f}", 2, windows), !nbsp_required(windows));
    }
}

/// REAL, 2026-10-02 journal, Claude Code 2.1.285, 83 columns, cursor 41;3: a named session's box.
#[test]
fn a_named_session_box_is_free() {
    let rule = |n: usize| "\u{2500}".repeat(n);
    let top = format!("{} named-session \u{2500}", rule(49));
    let prompt = "\u{276f}\u{a0}".to_string();
    for windows in [false, true] {
        assert!(prompt_free_on(&[top.clone(), prompt.clone(), rule(83)], Some((1, 2)), "claude", windows));
    }
    let glued = format!("{}label\u{2500}", rule(10)); // no spaces around the label: not a border
    let two = format!("{} a {} b \u{2500}", rule(10), rule(10)); // two labels: not a border
    let inner = format!("{} a\u{2500}b \u{2500}", rule(10)); // a label containing the rule glyph
    let wide = " a label as wide as the line ".to_string(); // no rule left on either side
    for bad in [glued, two, inner, wide] {
        assert!(!prompt_free_on(&[bad, prompt.clone(), rule(83)], Some((1, 2)), "claude", false));
    }
}
const P_MAIN: &str = "  ● main";
const P_SUB: &str = "  ◯ general-purpose  background task                    13s · ↓ 41.0k tokens";
const F_LEADER: &str = "  ⏵⏵ auto mode on · 1 shell · ← for agents";
const F_DOWN: &str = "  ⏵⏵ auto mode on · 1 shell";
const F_SELECT: &str = "  ↑/↓ to select";
const F_VIEW: &str = "  Enter to view · x to stop";
/// The cursor in the probe frames' prompt row, where Claude Code leaves it in an empty prompt.
const AT: (u16, u16) = (16, 2);

fn rule80() -> String {
    "\u{2500}".repeat(80)
}

/// REAL layout, scrubbed: the 2026-10-02 probe captures (Claude Code 2.1.287, 80 columns, one background
/// agent). Rows 15-17 are the box, 18-19 the statusline, 20 the footer, 22-23 the agents panel.
fn probe(top: &str, prompt: &str, footer: &str, panel: [&str; 2]) -> Vec<String> {
    let rule = rule80();
    let mut f = vec![String::new(); 15];
    for l in [top, prompt, rule.as_str(), "  Opus 5.5 [00000000] acct ·think:xhigh | v2.1.287 | demo:main", "  Session: 51k (in:51k out:1k) | $0.00", footer, "", panel[0], panel[1], ""] {
        f.push(l.to_string());
    }
    f
}

/// The stuck-wake record's shape: a 4x80 screen, the prompt between two rules, the cursor at column 3 (1-based).
fn parsed(prompt: &str) -> vt100_ctt::Parser {
    let rule = "\u{2500}".repeat(80);
    let mut p = vt100_ctt::Parser::new(4, 80, 0);
    p.process(format!("{rule}\r\n{prompt}\r\n{rule}\x1b[2;3H").as_bytes());
    p
}

#[test]
fn the_leader_view_reads_free() {
    // Capture 10, and the same box with a named session's label in its top border.
    let named = format!("{} named-session \u{2500}", "\u{2500}".repeat(64));
    for top in [rule80(), named] {
        for windows in [false, true] {
            assert_eq!(refused_on(&probe(&top, "\u{276f}\u{a0}", F_LEADER, [P_MAIN, P_SUB]), Some(AT), "claude", windows), None);
        }
    }
}

#[test]
fn typed_into_main_table() {
    const L: &str = "[sot-comm] you have mail: run comm-poll.sh";
    let at = |col: u16| Some((16, col));
    let typed = |top: &str, prompt: &str, footer: &str, panel: [&str; 2], cur, windows| typed_refusal(&probe(top, prompt, footer, panel), cur, "claude", windows, L).is_none();
    let line = format!("\u{276f}\u{a0}{L}");
    // The line in a leader box, wherever the cursor sits on the prompt row.
    assert!(typed(&rule80(), &line, F_LEADER, [P_MAIN, P_SUB], at(2 + L.len() as u16), false));
    assert!(typed(&rule80(), &line, F_LEADER, [P_MAIN, P_SUB], at(2), false));
    assert!(typed_refusal(&boxed(&line), Some((1, 44)), "claude", false, L).is_none());
    // A focused panel under the box.
    assert!(!typed(&rule80(), &line, F_SELECT, [P_MAIN, "❯ ◯ general-purpose"], at(2), false));
    // The cursor on a panel row, off the box.
    assert!(!typed(&rule80(), &line, F_LEADER, [P_MAIN, P_SUB], Some((22, 0)), false));
    // The box empty, the line plus more, other text, no NBSP.
    assert!(!typed(&rule80(), "\u{276f}\u{a0}", F_LEADER, [P_MAIN, P_SUB], at(2), false));
    assert!(!typed(&rule80(), &format!("{line} and more"), F_LEADER, [P_MAIN, P_SUB], at(2), false));
    assert!(!typed(&rule80(), &format!("\u{276f}\u{a0}hello {L}"), F_LEADER, [P_MAIN, P_SUB], at(2), false));
    assert!(!typed(&rule80(), &format!("\u{276f} {L}"), F_LEADER, [P_MAIN, P_SUB], at(2), false));
    // A sub-agent's labelled box, its panel focused on it.
    let top = format!("{} background task \u{2500}", "\u{2500}".repeat(62));
    let viewing = ["  ◯ main", "❯ ● general-purpose  background task                    13s · ↓ 41.0k tokens"];
    assert!(!typed(&top, &line, F_SELECT, viewing, at(2), false));
    // Windows: either glyph; the NBSP or, while it is not required there, a space.
    for glyph in ["\u{276f}", ">"] {
        assert!(typed(&rule80(), &format!("{glyph}\u{a0}{L}"), F_LEADER, [P_MAIN, P_SUB], at(2), true));
        assert_eq!(typed(&rule80(), &format!("{glyph} {L}"), F_LEADER, [P_MAIN, P_SUB], at(2), true), !nbsp_required(true));
        assert!(!typed(&rule80(), &format!("{glyph}{L}"), F_LEADER, [P_MAIN, P_SUB], at(2), true));
        assert!(!typed(&rule80(), &format!("{glyph}\u{a0}{L} x"), F_LEADER, [P_MAIN, P_SUB], at(2), true));
    }
    // The line followed by Claude Code's dim suggestion: dim cells read blank, so the line still sits alone.
    let rule = "\u{2500}".repeat(80);
    let mut p = vt100_ctt::Parser::new(3, 80, 0);
    p.process(format!("{rule}\r\n\u{276f}\u{a0}{L}\x1b[2m try this\x1b[22m\r\n{rule}\x1b[2;{}H", 3 + L.len()).as_bytes());
    let seen = free_test_lines(p.screen());
    assert_eq!(typed_refusal(&seen, Some(p.screen().cursor_position()), "claude", false, L), None);
}

#[test]
fn a_view_of_another_agent_is_refused() {
    // Capture 08: the agent's task in the top border, the dot and the panel cursor on the agent. The
    // placeholder `Message @general-purpose…` as the wake reads it if dim (blank), and as plain text.
    let top = format!("{} background task \u{2500}", "\u{2500}".repeat(62));
    let viewing = ["  ◯ main", "❯ ● general-purpose  background task                    13s · ↓ 41.0k tokens"];
    assert_eq!(refused_on(&probe(&top, "\u{276f}\u{a0}", F_SELECT, viewing), Some(AT), "claude", false), Some("agents panel focused or unrecognised"));
    assert_eq!(refused_on(&probe(&top, "\u{276f}\u{a0}Message @general-purpose…", F_SELECT, viewing), Some(AT), "claude", false), Some("input not empty"));
}

#[test]
fn the_dot_on_another_agent_is_refused() {
    // DERIVED from capture 08 with no panel cursor, under the leader footer.
    let dot_on_sub = ["  ◯ main", "  ● general-purpose  background task                    13s · ↓ 41.0k tokens"];
    assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}", F_LEADER, dot_on_sub), Some(AT), "claude", false), Some("the panel's dot is not on main"));
}

#[test]
fn panel_focus_is_refused() {
    // Captures 06 and 09 (the panel cursor on main) and 07 (on the agent): an Enter there opens a view.
    let on_sub = "❯ ◯ general-purpose  background task                    13s · ↓ 41.0k tokens";
    for (footer, panel) in [(F_SELECT, ["❯ ● main", P_SUB]), (F_VIEW, [P_MAIN, on_sub])] {
        assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}", footer, panel), Some(AT), "claude", false), Some("agents panel focused or unrecognised"));
    }
}

#[test]
fn an_unknown_panel_layout_is_refused() {
    // DERIVED: a panel with no line for main, and a dot on a name other than main.
    for panel in [[P_SUB, ""], ["  ● team-lead", P_SUB]] {
        assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}", F_LEADER, panel), Some(AT), "claude", false), Some("the panel's dot is not on main"));
    }
}

#[test]
fn a_draft_reads_not_free_wherever_its_cursor_sits() {
    // At Home the cursor sits where an empty prompt's does (col 2); only the text after the NBSP tells.
    for windows in [false, true] {
        assert_eq!(refused_on(&boxed("\u{276f}\u{a0}hello"), Some((1, 2)), "claude", windows), Some("input not empty"));
        assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}hello", F_LEADER, [P_MAIN, P_SUB]), Some(AT), "claude", windows), Some("input not empty"));
    }
}

#[test]
fn a_dim_suggestion_reads_empty_and_a_draft_does_not() {
    let p = parsed("\u{276f}\u{a0}\x1b[2mtry this\x1b[22m");
    let seen = free_test_lines(p.screen());
    assert_eq!(seen[1], "\u{276f}\u{a0}");
    assert_eq!(refused_on(&seen, Some(p.screen().cursor_position()), "claude", false), None);
    let p = parsed("\u{276f}\u{a0}try this");
    let seen = free_test_lines(p.screen());
    assert_eq!(seen[1], "\u{276f}\u{a0}try this");
    assert_eq!(refused_on(&seen, Some(p.screen().cursor_position()), "claude", false), Some("input not empty"));
    // Only the cursor's row is read this way: a dim line elsewhere keeps its text.
    let mut p = parsed("\u{276f}\u{a0}");
    p.process(b"\x1b[4;1H\x1b[2mfooter\x1b[22m\x1b[2;3H");
    assert_eq!(free_test_lines(p.screen())[3], "footer");
}

/// Claude Code 2.1.288's suggested reply, as captured from a live session on Linux (`run it` after a turn): the text after
/// the prompt's NBSP is SGR 2 and ends with a full reset (`ESC[0m`), not `ESC[22m`, and the cursor sits at column 2.
#[test]
fn cc288_a_suggested_reply_reads_empty() {
    let p = parsed("\u{276f}\u{a0}\x1b[2mrun it\x1b[0m\x1b[2;3H");
    let seen = free_test_lines(p.screen());
    assert_eq!(seen[1], "\u{276f}\u{a0}");
    assert_eq!(prompt_of(&seen, Some(p.screen().cursor_position()), "claude", false), Ok(Prompt::Empty));
}

#[test]
fn cc288_agent_view_off_panel_focus_is_refused() {
    for text in [
        include_str!("../../../tests/fixtures/comm_wake/cc288-avoff-2-down1.txt"),
        include_str!("../../../tests/fixtures/comm_wake/cc288-avoff-3-down2.txt"),
    ] {
        let (l, cur) = cc288(text, true);
        assert_eq!(cur, (23, 0));
        assert_eq!(refused_on(&l, Some(cur), "claude", false), Some("not in an input box"));
        // The cursor put back on the prompt: the panel cursor alone refuses.
        assert_eq!(refused_on(&l, Some((15, 2)), "claude", false), Some("agents panel focused or unrecognised"));
    }
}

#[test]
fn cc288_the_ticking_panel_is_not_held() {
    let (rest, _) = cc288(include_str!("../../../tests/fixtures/comm_wake/cc288-avoff-1-rest.txt"), true);
    let (later, _) = cc288(include_str!("../../../tests/fixtures/comm_wake/cc288-avoff-5-esc2.txt"), true);
    assert_ne!(rest, later);
    assert_eq!(held_rows(&rest, 15), held_rows(&later, 15));
}

#[test]
fn agent_view_focus_that_draws_nothing_reads_free() {
    // Capture 05 (2.1.287, agent view ON, the first ↓): focus left the input and nothing shows it. The named gap
    // of `panel_refusal`.
    assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}", F_DOWN, [P_MAIN, P_SUB]), Some(AT), "claude", false), None);
    assert_eq!(typed_refusal(&probe(&rule80(), &format!("\u{276f}\u{a0}{WAKE_LINE}"), F_DOWN, [P_MAIN, P_SUB]), Some(AT), "claude", false, WAKE_LINE), None);
}

#[test]
fn a_wake_line_with_more_or_less_is_a_draft() {
    for text in [format!("{WAKE_LINE} and more"), WAKE_LINE[..WAKE_LINE.len() - 5].to_string()] {
        let held = format!("\u{276f}\u{a0}{text}");
        assert_eq!(refused_on(&boxed(&held), Some((1, 2)), "claude", false), Some("input not empty"));
        assert!(typed_refusal(&boxed(&held), Some((1, 2)), "claude", false, WAKE_LINE).is_some());
    }
}

#[test]
fn free_test_lines_trims_spaces_and_keeps_a_no_break_space() {
    // A row off the cursor: the trailing ASCII spaces go, the no-break space after the glyph stays.
    let mut p = vt100_ctt::Parser::new(3, 80, 0);
    p.process("\x1b[3;1Ha\u{a0}  \x1b[1;1H".as_bytes());
    assert_eq!(free_test_lines(p.screen())[2], "a\u{a0}");
}
