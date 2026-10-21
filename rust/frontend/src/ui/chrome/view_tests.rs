use super::*;

#[test]
fn prompt_pinned_cells_have_attention_style() {
    let prompts = [
        NavPrompt::CreateFile { dir_node_id: "files:".into(), input: "name".into() },
        NavPrompt::ConfirmDelete { node_id: "files:a".into(), label: "a".into() },
        NavPrompt::ConfirmQuit { keep: false },
        NavPrompt::ScaleEntry { node_id: "files:a".into(), input: "1".into() },
    ];
    for prompt in prompts.iter().map(Some).chain(std::iter::once(None)) {
        for (width, height) in [(80, 8), (16, 8), (16, 1), (16, 0)] {
            let text = prompt.map(|p| match p {
                NavPrompt::ConfirmQuit { keep } => quit_prompt_line(*keep),
                _ => ("a wrapped navigation prompt with a choice".into(), "[choice]".into()),
            });
            let (list_h, rows, _) = nav_pinned_rows(text.as_ref().map(|(a,b)| (a.as_str(),b.as_str())),
                Some("notice"), None, width, height);
            let rect = ratatui::layout::Rect::new(0, 0, width as u16, height as u16);
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width as u16, height as u16)).unwrap();
            term.draw(|frame| render_nav_widgets(frame, Paragraph::new(""),
                ratatui::layout::Rect { height: list_h as u16, ..rect }, rows.clone(), rect, list_h, prompt.is_some())).unwrap();
            for (i, row) in rows.iter().enumerate() {
                for x in 0..row.chars().count().min(width) {
                    let cell = &term.backend().buffer()[(x as u16, (list_h + i) as u16)];
                    assert_eq!(cell.fg, if prompt.is_some() { Color::Yellow } else { Color::LightGreen }, "prompt foreground");
                    assert_eq!(cell.modifier.contains(Modifier::BOLD), prompt.is_some(), "prompt weight");
                }
            }
            if height == 1 && prompt.is_some() { assert!(rows[0].contains('['), "whole choice survives clipping"); }
            if height == 0 { assert!(rows.is_empty()); }
        }
    }
}

/// The REPL log's lines for `untrusted_lines_stay_apart_in_the_chrome`: code holding a tab, output and an error
/// holding a `\r\n`, and a value's MIME, an image's MIME and a page's address holding a `\n`.
fn untrusted_repl_lines() -> Vec<RtLine<'static>> {
    use crate::ui::drawer::repl::{lines::build_repl_lines, log::ReplEntry};
    use sot_protocol::ReplFrame::{Browser, Error, Image, Stdout, Value};
    let frames = vec![
        Stdout {
            text: "AA10\r\nBB10\n".to_string(),
        },
        Error {
            message: "AA11\r\nBB11".to_string(),
            stacktrace: Vec::new(),
        },
        Value {
            mime: "AA14\nBB14".to_string(),
            text: "v".to_string(),
        },
        Image {
            mime: "AA15\nBB15".to_string(),
            data_base64: String::new(),
            bytes: 3,
        },
        Browser {
            url: "http://AA16\nBB16/x".to_string(),
            open: false,
            fe: None,
        },
    ];
    let log = vec![ReplEntry {
        eval_id: 1,
        code: "AA09\tBB09".to_string(),
        frames,
        elapsed_ms: 0,
        in_flight: false,
        pkg_mode: false,
        origin: None,
    }];
    build_repl_lines(&log, &HashMap::new(), 0.0, 0.0, 8.0, 16.0, false).0
}

/// The chrome painted into a `size` buffer, row by row, with `status` and the nav rows `tree`, and every other
/// text from outside the window holding a line break or a tab between a marker `AAnn` and its `BBnn`. The pinned
/// preview's file name holds sixty more breaks, so its title is cut to fit.
fn paint_chrome(size: (u16, u16), status: &str, tree: Vec<NavRow>) -> Vec<String> {
    let preset = crate::ui::persist::settings::LayoutPreset::default_laptop();
    let status = status.to_string();
    let prompt = Some(("delete AA05\rBB05? [y/N]".to_string(), String::new()));
    let none: Option<String> = None;
    let concept = "c AA06\nBB06".to_string();
    let preview_name = Some(format!("dir/AA07\nBB07{}.jl", "\nzz".repeat(60)));
    let pinned = Some("files:dir".to_string());
    let host: HostKey = "local".to_string();
    let repl_input = "x = AA08\tBB08\ny".to_string();
    let repl_lines = untrusted_repl_lines();
    let (help_state, bindings, context) = (
        help::Help::default(),
        KeyBindings::defaults(),
        help::Context::default(),
    );
    let (battery, stamp) = (None, " fe 0 · be 0 ".to_string());
    let screen = crate::ui::agent_pane::blank_pane_screen(80, 24);
    let overlay = vec!["pty.open failed: AA12\nBB12".to_string()];
    let mut view = ChromeView {
        layout_preset: &preset,
        drawer_open: true,
        maximize_slot: None,
        nav_logo_row: false,
        focus: PaneFocus::NavTree,
        mode: Mode::default(),
        nav_prompt_line: &prompt,
        lease_notice: None,
        nav_line: &none,
        owed_line: &none,
        status: &status,
        nav_cursor_body_pos: 2,
        tree_empty: tree.is_empty(),
        contrast_dim: false,
        concept_status: &concept,
        last_key: &none,
        nav_has_cursor: !tree.is_empty(),
        nav_spill_active: false,
        preview_name: &preview_name,
        pinned_preview_node_id: &pinned,
        drawer: DrawerContent::Repl,
        monitor_host_connected: true,
        monitor_host_label: &host,
        repl_input: &repl_input,
        repl_lines: &repl_lines,
        repl_pkg_mode: false,
        help_state: &help_state,
        help_bindings: &bindings,
        help_context: &context,
        clock_now: chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
        battery: &battery,
        version_stamp: &stamp,
        version_skew: false,
        pty_screen: &screen,
        pane_overlay: &overlay,
        term_screen: None,
        owed_drawn: false,
        leaving_drawn: false,
        nav_scroll: 0,
        preview_cells: Default::default(),
        repl_cells: Default::default(),
        new_pane_rects: PaneRects::default(),
        new_repl_scroll: 0,
        repl_scrollback_cells: Default::default(),
        repl_window: (0, 0),
        pty_size_observed: (0, 0),
        term_size_observed: (0, 0),
    };
    let (w, h) = size;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|frame| view.paint(frame, tree, &mut Vec::new()))
        .unwrap();
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

/// Text from outside the window (a daemon's error, an agent's summary, a file name, a paste) that holds a line
/// break or another control character keeps its pieces apart wherever the chrome draws it: no cell holds a
/// control character, and each `AAnn` is followed by its `BBnn` on the next row, or on the same row after the
/// separator only one row has room for (`⏎` for a line break, a space for a tab). A title cut to fit keeps its
/// marker.
#[test]
fn untrusted_lines_stay_apart_in_the_chrome() {
    let status = "s AA01\nBB01 AA02\r\nBB02 AA03\rBB03 AA04\tBB04";
    let row = "> . AA13\nBB13".to_string();
    let rows = paint_chrome(
        (200, 60),
        status,
        vec![(row, true, false, false, None, 0.0, false, false)],
    );
    let mut failures: Vec<String> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains(char::is_control))
        .map(|(y, row)| format!("row {y} holds a control character: {row:?}"))
        .collect();
    // (marker, how its two pieces must sit): None = on consecutive rows, Some(sep) = on one row around `sep`.
    let cases: &[(u8, Option<&str>)] = &[
        (1, None),
        (2, None),
        (3, None),
        (4, Some(" ")),
        (5, None),
        (6, Some(" ⏎ ")),
        (7, Some(" ⏎ ")),
        (8, Some(" ")),
        (9, Some(" ")),
        (10, None),
        (11, None),
        (12, Some(" ⏎ ")),
        (13, Some(" ⏎ ")),
        (14, Some(" ⏎ ")),
        (15, Some(" ⏎ ")),
        (16, Some(" ⏎ ")),
    ];
    for &(n, sep) in cases {
        let (a, b) = (format!("AA{n:02}"), format!("BB{n:02}"));
        let ra = rows.iter().position(|r| r.contains(&a));
        let rb = rows.iter().position(|r| r.contains(&b));
        let apart = match sep {
            None => ra.is_some() && rb == ra.map(|r| r + 1),
            Some(sep) => rows.iter().any(|r| r.contains(&format!("{a}{sep}{b}"))),
        };
        if !apart {
            let shown: Vec<&String> = ra.iter().chain(rb.iter()).map(|&r| &rows[r]).collect();
            failures.push(format!("{a} and {b} are not apart: {shown:?}"));
        }
    }
    if !rows.iter().any(|r| r.contains("[pinned *]")) {
        failures.push("the preview title lost its `[pinned *]` marker".to_string());
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A status longer than the nav pane (a Quarto failure keeps up to 64 KiB: what failed, its ERROR lines with the
/// root cause first, Quarto's last lines, then a hint) shows its lines whole by priority, never rows of one: at
/// a 36-cell nav (200x60) what failed, the whole root-cause line and the whole hint; at a 21-cell nav (120x36,
/// ten rows) what failed and the start of the root cause, and no piece of the hint. The tree keeps the rest.
#[test]
fn a_long_status_keeps_what_failed_its_cause_and_its_hint_whole() {
    const HINT: &str = "Quarto read an incomplete Julia server transport line. Quarto 1.9.38 and later wait up to two seconds for the newline, so slow server startup can still cause this error. If using an older Quarto, upgrading to 1.9.38 or later may help.";
    let cause = "ERROR: Julia server returned error after receiving \"isready\" command: Read invalid transport file that did not end with a newline";
    let flood: Vec<String> = (0..300).map(|n| format!("ERROR flood {n:03}")).collect();
    let frames: Vec<String> = (0..10).map(|n| format!("stack frame {n:02}")).collect();
    let status = format!(
        "quarto.open failed · quarto render failed (execute=true):\n{cause}\n{}\n{}\n{HINT}",
        flood.join("\n"),
        frames.join("\n")
    );
    // (window, the texts that must show, the texts that must not)
    let cases: [((u16, u16), &[&str], &[&str]); 2] = [
        ((200, 60), &["status: quarto.open", "ERROR: Julia server", "newline", "not shown", "Quarto read", "may help.", "(no tree yet)"], &[]),
        ((120, 36), &["status: quarto.open", "ERROR: Julia", "not shown", "(no tree yet)"], &["Quarto read", "may help."]),
    ];
    for (size, shown, hidden) in cases {
        let rows = paint_chrome(size, &status, Vec::new());
        let missing: Vec<&str> = shown.iter().copied().filter(|w| !rows.iter().any(|r| r.contains(w))).collect();
        let leaked: Vec<&str> = hidden.iter().copied().filter(|w| rows.iter().any(|r| r.contains(w))).collect();
        assert!(missing.is_empty() && leaked.is_empty(), "{size:?}: not shown {missing:?}, shown {leaked:?} in {rows:#?}");
    }
}
