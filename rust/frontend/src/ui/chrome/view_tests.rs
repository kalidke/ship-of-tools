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
            let body = NavBody {
                list_h,
                pinned: rows.clone(),
                header: (ratatui::layout::Rect::default(), Paragraph::new("")),
                tree: (ratatui::layout::Rect { height: list_h as u16, ..rect }, Paragraph::new("")),
            };
            term.draw(|frame| render_nav_widgets(frame, body, rect, prompt.is_some())).unwrap();
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
    use crate::ui::drawer::repl::{presentation::ReplPresentation, log::ReplEntry};
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
        own_frames: Vec::new(),
    }];
    let row = ("host".into(), "row".into());
    ReplPresentation::<()>::default().build(&row, &log, false, 8.0, 16.0, 0).0
}

/// The chrome painted into a `size` buffer, row by row, with `status` and the nav rows `tree`, and every other
/// text from outside the window holding a line break or a tab between a marker `AAnn` and its `BBnn`. The pinned
/// preview's file name holds sixty more breaks, so its title is cut to fit.
fn paint_chrome(size: (u16, u16), status: &str, tree: Vec<NavRow>) -> Vec<String> {
    paint_chrome_repl(size, status, tree,
        ("x = AA08\tBB08\ny".into(), untrusted_repl_lines(), 0, DrawerContent::Repl)).0
}

fn paint_chrome_repl(
    size: (u16, u16),
    status: &str,
    tree: Vec<NavRow>,
    repl: (String, Vec<RtLine<'static>>, u16, DrawerContent),
) -> (Vec<String>, u16, ratatui::layout::Rect, (usize, usize)) {
    let (rows, scroll, area, window, ..) = paint_chrome_chat(size, status, tree, repl, None);
    (rows, scroll, area, window)
}

/// `paint_chrome_repl` with the Chat drawer's compose text, and the drawer's rect and compose rows drawn besides.
fn paint_chrome_chat(
    size: (u16, u16),
    status: &str,
    tree: Vec<NavRow>,
    repl: (String, Vec<RtLine<'static>>, u16, DrawerContent),
    chat_compose: Option<&crate::ui::drawer::chat::ComposeText>,
) -> (Vec<String>, u16, ratatui::layout::Rect, (usize, usize), ratatui::layout::Rect, u16) {
    let (rows, scroll, area, window, cells, foot, ..) =
        paint_chrome_nav(size, status, tree, repl, chat_compose, NavFrame::default());
    (rows, scroll, area, window, cells, foot)
}

/// What a frame of the nav pane starts from: the spill on or off, the nav scroll, and, for `alone: Some(h)`, the
/// nav pane painted alone, 36 columns by `h` rows with no prompt at its foot, so its list is `h` rows.
#[derive(Clone, Copy, Default)]
struct NavFrame<'a> {
    spill: bool,
    scroll: u16,
    alone: Option<u16>,
    /// What the Chat drawer shows in its body instead of records.
    body: Option<&'a crate::ui::drawer::chat::Body>,
    /// The drawer at full height instead of its own.
    full: bool,
    /// The REPL drawer has the focus, and is in pkg mode.
    repl_focused: bool,
    repl_pkg: bool,
    /// Where to leave the painted buffer, for a test that reads cell colors.
    keep: Option<&'a std::cell::RefCell<Option<ratatui::buffer::Buffer>>>,
}

/// `paint_chrome_chat` from `nav`, and the nav pane's rect, the spill's segments and the nav scroll the frame ends
/// on besides.
fn paint_chrome_nav(
    size: (u16, u16),
    status: &str,
    tree: Vec<NavRow>,
    repl: (String, Vec<RtLine<'static>>, u16, DrawerContent),
    chat_compose: Option<&crate::ui::drawer::chat::ComposeText>,
    nav: NavFrame<'_>,
) -> (Vec<String>, u16, ratatui::layout::Rect, (usize, usize), ratatui::layout::Rect, u16, ratatui::layout::Rect, Vec<NavSpillSeg>, u16) {
    let (repl_input, repl_lines, scroll, drawer) = repl;
    let preset = crate::ui::persist::settings::LayoutPreset::default_laptop();
    let status = status.to_string();
    let prompt = nav.alone.is_none().then(|| ("delete AA05\rBB05? [y/N]".to_string(), String::new()));
    let none: Option<String> = None;
    let concept = "c AA06\nBB06".to_string();
    let preview_name = Some(format!("dir/AA07\nBB07{}.jl", "\nzz".repeat(60)));
    let pinned = Some("files:dir".to_string());
    let host: HostKey = "local".to_string();
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
        drawer_full: nav.full,
        maximize_slot: None,
        nav_logo_row: false,
        focus: if nav.repl_focused { PaneFocus::Repl } else { PaneFocus::NavTree },
        mode: Mode::default(),
        nav_prompt_line: &prompt,
        lease_notice: None,
        nav_line: &none,
        owed_line: &none,
        status: &status,
        tree_empty: tree.is_empty(),
        contrast_dim: false,
        concept_status: &concept,
        last_key: &none,
        nav_spill_active: nav.spill,
        preview_name: &preview_name,
        pinned_preview_node_id: &pinned,
        drawer,
        chat_body: nav.body,
        chat_title: " chat · C1 reading… ",
        chat_compose,
        chat_foot: 0,
        chat_body_end: (0, 0),
        monitor_host_connected: true,
        monitor_host_label: &host,
        repl_input: &repl_input,
        repl_lines: &repl_lines,
        repl_pkg_mode: nav.repl_pkg,
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
        nav_scroll: nav.scroll,
        preview_cells: Default::default(),
        repl_cells: Default::default(),
        new_pane_rects: PaneRects::default(),
        new_repl_scroll: scroll,
        repl_scrollback_cells: Default::default(),
        repl_window: (0, 0),
        pty_size_observed: (0, 0),
        term_size_observed: (0, 0),
    };
    let (w, h) = nav.alone.map_or(size, |h| (36, h));
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    let mut segs = Vec::new();
    match nav.alone {
        None => term.draw(|frame| view.paint(frame, tree, &mut segs)).unwrap(),
        Some(_) => term.draw(|frame| {
            let area = frame.area();
            view.new_pane_rects.nav = area;
            let body = view.nav_body(area, ratatui::layout::Rect::default(), tree, &mut segs);
            render_nav_widgets(frame, body, area, false);
        }).unwrap(),
    };
    let buf = term.backend().buffer();
    if let Some(keep) = nav.keep {
        *keep.borrow_mut() = Some(buf.clone());
    }
    let rows = (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    (rows, view.new_repl_scroll, view.repl_scrollback_cells, view.repl_window, view.repl_cells, view.chat_foot, view.new_pane_rects.nav, segs, view.nav_scroll)
}

#[test]
fn repl_drawer_layout_uses_the_owner_window() {
    for drawer in [DrawerContent::Repl, DrawerContent::Monitor, DrawerContent::Help, DrawerContent::Terminal] {
        for proposed in [4, 999] {
            let lines = (0..100).map(|i| RtLine::from(format!("log{i:02}"))).collect();
            let (rows, scroll, area, window) = paint_chrome_repl((200, 60), "ready", Vec::new(),
                ("aa\nbb\ncc".into(), lines, proposed, drawer));
            let height = area.height as usize;
            assert!(height > 0);
            let expected = proposed.min((100 - height) as u16);
            assert_eq!(scroll, expected);
            let end = 100 - expected as usize;
            assert_eq!(window, (end - height, end));
            let mut model = ReplModel::default();
            model.commit_rendered_scroll(proposed, scroll);
            assert_eq!(model.scroll(), expected);
            if drawer == DrawerContent::Repl {
                for (i, y) in (area.y..area.bottom()).enumerate() {
                    assert!(rows[y as usize].contains(&format!("log{:02}", window.0 + i)));
                }
                assert!(rows[area.bottom() as usize].contains("julia> aa"), "input starts after scrollback");
            }
        }
    }
}

/// The live prompt as the chrome paints it, in the REPL drawer's rows: Julia's color and weight for each mode while
/// the pane has the focus (the cursor block in the prompt's color), the unfocused gray without it.
#[test]
fn the_live_prompt_is_painted_in_julias_color_and_weight() {
    for (pkg, focused, text, color, cursor) in [
        (false, true, "julia> xy", Color::Green, Some(Color::Green)),
        (true, true, "pkg> xy", Color::Blue, Some(Color::Blue)),
        (false, false, "julia> xy", Color::DarkGray, None),
        (true, false, "pkg> xy", Color::DarkGray, None),
    ] {
        let kept = std::cell::RefCell::new(None);
        let nav = NavFrame { repl_focused: focused, repl_pkg: pkg, keep: Some(&kept), ..NavFrame::default() };
        let (rows, ..) = paint_chrome_nav((120, 40), "ready", Vec::new(),
            ("xy".into(), Vec::new(), 0, DrawerContent::Repl), None, nav);
        let buf = kept.borrow().clone().unwrap();
        let y = rows.iter().position(|r| r.contains(text)).unwrap_or_else(|| panic!("no {text:?} row in {rows:?}"));
        let x = rows[y].find(text).map(|byte| rows[y][..byte].chars().count()).unwrap() as u16;
        let prompt = &buf[(x, y as u16)];
        assert_eq!((prompt.fg, prompt.modifier.contains(Modifier::BOLD)), (color, true), "{text} focused={focused}");
        let block = (0..120u16).find(|&bx| buf[(bx, y as u16)].symbol() == "\u{2588}");
        assert_eq!(block.map(|bx| buf[(bx, y as u16)].fg), cursor, "{text} focused={focused}: cursor");
    }
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

#[test]
fn the_chat_drawer_draws_its_body_and_no_other_drawer_does() {
    use crate::ui::drawer::chat::Body;
    let body = Body::words("this project has no channel: press Enter to pick one");
    let drawn = |drawer| {
        let frame = NavFrame { body: Some(&body), ..NavFrame::default() };
        let (rows, ..) = paint_chrome_nav((200, 60), "ready", Vec::new(), (String::new(), Vec::new(), 0, drawer), None, frame);
        rows.iter().any(|r| r.contains("this project has no channel: press Enter to pick one"))
    };
    assert!(drawn(DrawerContent::Chat));
    for other in [DrawerContent::Repl, DrawerContent::Terminal, DrawerContent::Monitor, DrawerContent::Help] {
        assert!(!drawn(other), "{other:?}");
    }
}

#[test]
fn the_chat_drawer_draws_its_compose_rows_at_its_foot_and_no_other_drawer_does() {
    use crate::ui::drawer::chat::ComposeText;
    let text = ComposeText::Draft { lines: vec!["hello".into(), "wo".into()], to: Some("to C1".into()) };
    let drawn = |drawer, short: u16| {
        let (rows, _, _, _, cells, foot) =
            paint_chrome_chat((120, 40 - short), "ready", Vec::new(), (String::new(), Vec::new(), 0, drawer), Some(&text));
        (rows, cells, foot)
    };
    let (rows, cells, foot) = drawn(DrawerContent::Chat, 0);
    assert_eq!(foot, 3);
    let last = cells.bottom() as usize;
    for (row, want) in [(last - 3, "> hello"), (last - 2, "  wo"), (last - 1, "to C1")] {
        assert!(rows[row].contains(want), "{row}: {:?}", rows[row]);
    }
    assert!(!rows[last - 4].contains("hello"), "above the foot is the records' room");
    for other in [DrawerContent::Repl, DrawerContent::Monitor, DrawerContent::Help, DrawerContent::Terminal] {
        let (rows, _, foot) = drawn(other, 0);
        assert_eq!(foot, 0, "{other:?}");
        assert!(!rows.iter().any(|r| r.contains("> hello")), "{other:?}");
    }
}

/// Each tenant draws into the rect it is given, so a full-height drawer is drawn as the rect it stands in: the
/// Terminal's size is read from that rect on every frame (`term_size_observed`), the Chat's foot sits at its bottom,
/// the REPL's input follows its scrollback, and the Monitor's chart is planned from it.
#[test]
fn every_tenant_fills_the_rect_a_full_height_drawer_stands_in() {
    use crate::ui::drawer::chat::ComposeText;
    let text = ComposeText::Draft { lines: vec!["hello".into()], to: Some("to C1".into()) };
    let lines: Vec<RtLine<'static>> = (0..100).map(|i| RtLine::from(format!("log{i:02}"))).collect();
    for drawer in [DrawerContent::Repl, DrawerContent::Terminal, DrawerContent::Monitor, DrawerContent::Chat] {
        let mut heights = Vec::new();
        for full in [false, true] {
            let frame = NavFrame { full, ..NavFrame::default() };
            let compose = (drawer == DrawerContent::Chat).then_some(&text);
            let (rows, _, _, window, cells, foot, ..) = paint_chrome_nav((200, 60), "ready", Vec::new(),
                ("aa".into(), lines.clone(), 0, drawer), compose, frame);
            heights.push(cells.height);
            if !full { continue; }
            assert_eq!((cells.y, cells.height, cells.x), (1, 58, 1), "{drawer:?}: the columns' whole height");
            let last = cells.bottom() as usize;
            match drawer {
                DrawerContent::Repl => {
                    assert_eq!(window.1 - window.0, 57, "the scrollback fills the taller rect");
                    assert!(rows[last - 1].contains("julia> aa"), "{:?}", rows[last - 1]);
                }
                DrawerContent::Chat => {
                    assert_eq!(foot, 2);
                    assert!(rows[last - 2].contains("> hello") && rows[last - 1].contains("to C1"));
                }
                _ => {}
            }
        }
        assert!(heights[1] > heights[0] * 2, "{drawer:?}: {heights:?}");
    }
}

#[test]
fn the_chat_drawer_title_is_the_one_it_is_given_and_no_other_drawer_shows_it() {
    let titled = |drawer| {
        let (rows, ..) = paint_chrome_repl((200, 60), "ready", Vec::new(), (String::new(), Vec::new(), 0, drawer));
        rows.iter().any(|r| r.contains("chat · C1 reading…"))
    };
    assert!(titled(DrawerContent::Chat));
    for other in [DrawerContent::Repl, DrawerContent::Terminal, DrawerContent::Monitor, DrawerContent::Help] {
        assert!(!titled(other), "{other:?}");
    }
}

/// The nav pane's cells of each of its rows, the trailing blanks cut.
fn nav_cells(rows: &[String], nav: ratatui::layout::Rect) -> Vec<String> {
    rows[nav.y as usize..nav.bottom() as usize]
        .iter()
        .map(|r| r.chars().skip(nav.x as usize).take(nav.width as usize).collect::<String>().trim_end().to_string())
        .collect()
}

/// `n` tree rows, `file000.jl` on, each name `pad` x's longer, with the cursor on row `cursor`.
fn file_rows(n: usize, cursor: usize, pad: usize) -> Vec<NavRow> {
    (0..n)
        .map(|i| {
            let caret = if i == cursor { ">" } else { " " };
            (format!("{caret} · file{i:03}{}.jl", "x".repeat(pad)), i == cursor, false, false, None, 0.0, false, false)
        })
        .collect()
}

/// One frame of the nav pane from `scroll`: the rows painted, the pane's rect, the spill's segments and the scroll
/// the frame ends on. The nav rows are mode-independent (a mode reaches them only through the Sessions contrast
/// lever, off here), so the fixture paints in the default mode.
fn paint_nav(size: (u16, u16), status: &str, tree: Vec<NavRow>, nav: NavFrame) -> (Vec<String>, ratatui::layout::Rect, Vec<NavSpillSeg>, u16) {
    let repl = (String::new(), Vec::new(), 0, DrawerContent::Repl);
    let (rows, .., rect, segs, scroll) = paint_chrome_nav(size, status, tree, repl, None, nav);
    (rows, rect, segs, scroll)
}

/// The nav pane alone, its list `list` rows, from `scroll`.
fn paint_list(list: u16, status: &str, tree: Vec<NavRow>, scroll: u16) -> (Vec<String>, ratatui::layout::Rect, Vec<NavSpillSeg>, u16) {
    paint_nav((0, 0), status, tree, NavFrame { scroll, alone: Some(list), ..NavFrame::default() })
}

/// With a tree longer than the pane and the cursor on its last row, the status's lines and the spacer under them
/// are the pane's first rows and the cursor's row is drawn beneath them.
#[test]
fn a_long_tree_keeps_the_status_on_the_panes_first_rows_and_draws_the_cursor() {
    for size in [(200, 60), (120, 30)] {
        let (rows, nav, ..) = paint_nav(size, "ready\nsecond line", file_rows(300, 299, 0), NavFrame::default());
        let shown = nav_cells(&rows, nav);
        assert_eq!(&shown[..3], ["status: ready", "second line", ""], "{size:?}: {shown:#?}");
        let cursor = shown.iter().position(|r| r == "> · file299.jl");
        assert!(cursor.is_some_and(|y| y > 2), "{size:?}: the cursor's row is not drawn: {shown:#?}");
        assert!(!shown[3..].iter().any(|r| r.starts_with("status:")), "{size:?}: a second status");
    }
}

/// The spill's segments name the tree rows actually drawn, each at the row it is drawn on, and never a status
/// row, whatever the scroll.
#[test]
fn the_spill_names_the_tree_rows_drawn_where_they_are_drawn() {
    for cursor in [0, 40, 150, 299] {
        let (rows, nav, segs, _) = paint_nav((200, 60), "ready\nsecond line", file_rows(300, cursor, 80), NavFrame { spill: true, ..NavFrame::default() });
        let shown = nav_cells(&rows, nav);
        let drawn: Vec<u16> = (0..shown.len()).filter(|&y| shown[y].contains(" · file")).map(|y| nav.y + y as u16).collect();
        let named: Vec<u16> = segs.iter().map(|s| s.row).collect();
        assert!(!drawn.is_empty() && named == drawn, "{cursor}: segments {named:?}, drawn tree rows {drawn:?}");
        for seg in &segs {
            let cells = &shown[(seg.row - nav.y) as usize];
            assert_eq!(seg.x, nav.x);
            assert!(seg.text.starts_with(cells.as_str()), "{cursor}: {:?} over {cells:?}", seg.text);
        }
        let cursor_row = format!("> · file{cursor:03}");
        assert!(shown.iter().any(|r| r.starts_with(&cursor_row)), "{cursor}: cursor not drawn");
    }
}

/// A tree that fits sits where it sat when the status scrolled with it: the status (an empty one is its label
/// alone, one row), a spacer, the tree, then the annotation and key lines, each with a blank row before it.
#[test]
fn a_short_tree_sits_under_the_status_as_before() {
    for (status, first) in [("ready", "status: ready"), ("", "status:")] {
        let (rows, nav, ..) = paint_nav((200, 60), status, file_rows(5, 4, 0), NavFrame::default());
        let shown = nav_cells(&rows, nav);
        let want = [first, "", "  · file000.jl", "  · file001.jl", "  · file002.jl", "  · file003.jl", "> · file004.jl", "", "c AA06 ⏎ BB06", "", "key: (none)"];
        assert_eq!(&shown[..want.len()], want, "{status:?}: {shown:#?}");
    }
}

/// In a pane too short for the status, its spacer and a tree row, the status gives way from its foot: the tree
/// keeps a row and the cursor is drawn on it, wherever it is.
#[test]
fn a_short_pane_keeps_a_tree_row_for_the_cursor() {
    for h in 8..=14 {
        for cursor in [0, 150, 299] {
            let (rows, nav, ..) = paint_nav((200, h), "ready\nsecond line", file_rows(300, cursor, 0), NavFrame::default());
            let shown = nav_cells(&rows, nav);
            let cursor_row = format!("> · file{cursor:03}.jl");
            assert!(shown.iter().any(|r| *r == cursor_row), "{h} {cursor}: {shown:#?}");
            assert!(shown[0].starts_with("status: ready") || shown[0] == cursor_row, "{h} {cursor}: {shown:#?}");
        }
    }
}

/// Frame to frame, with the scroll carried as the window carries it: over list heights 1 to 20, statuses of 0, 1
/// and 3 lines, stale starting scrolls (an older build's, past the end) and a cursor that walks down, up and
/// jumps, every frame draws the cursor (or, with no cursor, the tree's first row), and painting again from the
/// scroll a frame ends on ends on that same scroll.
#[test]
fn the_cursor_is_drawn_every_frame_and_the_scroll_settles() {
    let walk = [Some(30), Some(31), Some(32), Some(31), Some(30), Some(29), Some(0), Some(1), Some(59), Some(58), None, Some(0), Some(59), Some(0)];
    for list in 1..=20u16 {
        for status in ["", "ready", "one\ntwo\nthree"] {
            for start in [0u16, 5, 30, u16::MAX] {
                let mut scroll = start;
                for (step, cursor) in walk.iter().enumerate() {
                    let tree = file_rows(60, cursor.unwrap_or(usize::MAX), 0);
                    let (rows, nav, _, end) = paint_list(list, status, tree.clone(), scroll);
                    let shown = nav_cells(&rows, nav);
                    let want = match cursor {
                        Some(c) => format!("> · file{c:03}.jl"),
                        None => "  · file000.jl".to_string(),
                    };
                    let at = format!("list {list}, status {status:?}, start {start}, step {step} ({cursor:?}), scroll {scroll} -> {end}");
                    assert!(shown.iter().any(|r| *r == want), "{at}: {want:?} not drawn in {shown:#?}");
                    let (.., again) = paint_list(list, status, tree, end);
                    assert_eq!(again, end, "{at}: a repaint moved the scroll");
                    scroll = end;
                }
            }
        }
    }
}

/// The workspace picker's rows (its path, the two legend rows, the account row when an account is shown, then the
/// entries) sit ahead of its cursor in the scrolled body: the cursor's entry is drawn wherever it is, in a list of
/// one row up, frame to frame.
#[test]
fn the_pickers_cursor_is_drawn_under_its_own_header_rows() {
    let row = |text: String, selected: bool, attention: bool| -> NavRow { (text, selected, false, false, None, 0.0, false, attention) };
    let picker = |cursor: usize, account: bool| -> Vec<NavRow> {
        let mut rows = vec![
            row("workspace picker · /home/dir".into(), false, false),
            row("  → into · ← up · ↓ move · Esc cancel".into(), false, false),
            row("  n Claude · b bare · x Codex".into(), false, false),
        ];
        if account {
            rows.push(row("  account: work · a next".into(), false, true));
        }
        rows.extend((0..300).map(|i| row(format!("{} ▸ dir{i:03}/", if i == cursor { ">" } else { " " }), i == cursor, false)));
        rows
    };
    for list in 1..=8u16 {
        for account in [false, true] {
            let mut scroll = 0;
            for cursor in [0, 1, 2, 150, 299, 0] {
                let (rows, nav, _, end) = paint_list(list, "ready", picker(cursor, account), scroll);
                let shown = nav_cells(&rows, nav);
                let want = format!("> ▸ dir{cursor:03}/");
                assert!(shown.iter().any(|r| *r == want), "list {list}, account {account}, cursor {cursor}: {shown:#?}");
                scroll = end;
            }
        }
    }
}
