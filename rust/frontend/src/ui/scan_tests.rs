//! The window crate's own source, for the tests that scan it (00-common item 18), read through
//! `sot_log::test_scan::rust_sources()`.

pub(super) fn crate_source() -> String {
    let mut files: Vec<(String, String)> = sot_log::test_scan::rust_sources()
        .into_iter()
        .filter(|(rel, _)| rel.starts_with("rust/frontend/src/"))
        .collect();
    assert!(!files.is_empty(), "no .rs file found under rust/frontend/src");
    files.sort();
    files.into_iter().map(|(_, text)| text).collect()
}

/// The focus writes in `src` other than the spaced one
/// `focus_written_only_by_set_focus` counts: unspaced, swapped or
/// replaced through `mem`, or named in a destructuring assignment. Each
/// pattern is split so this test's own text does not match.
fn stray_focus_writes(src: &str) -> Vec<String> {
    let src = src.replace("\r\n", "\n");
    let mut hits = Vec::new();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    // A `&mut` borrow of the field, which can write it anywhere.
    for (i, _) in src.match_indices("&mut ") {
        let path: String = src[i + 5..].chars().take_while(|&c| ident(c) || c == '.').collect();
        if path.ends_with(".focus") {
            hits.push(src[i..].lines().next().unwrap_or_default().to_string());
        }
    }
    let tight = [".focus", "="].concat();
    for (i, _) in src.match_indices(tight.as_str()) {
        if !src[i + tight.len()..].starts_with('=') {
            hits.push(src[i..].lines().next().unwrap_or_default().to_string());
        }
    }
    for f in [["mem::", "swap("].concat(), ["mem::", "replace("].concat()] {
        for (i, _) in src.match_indices(f.as_str()) {
            let call = &src[i..i + src[i..].find(';').unwrap_or(src.len() - i)];
            if call.contains(".focus") {
                hits.push(call.to_string());
            }
        }
    }
    let names_focus = |lhs: &str| {
        lhs.match_indices("focus").any(|(i, _)| !lhs[..i].ends_with(ident) && !lhs[i + 5..].starts_with(ident))
    };
    let mut off = 0;
    for line in src.split_inclusive('\n') {
        let at = off + line.len() - line.trim_start().len();
        off += line.len();
        let line = line.trim_end_matches('\n');
        let t = line.trim_start();
        if t.starts_with("let ") {
            continue;
        }
        // The first `=` that assigns: not `==`, `=>`, `!=`, `<=` or `>=`.
        let Some(eq) = t.char_indices().map(|(i, _)| i).find(|&i| {
            t[i..].starts_with('=') && !t[i + 1..].starts_with(['=', '>']) && !t[..i].ends_with(['=', '!', '<', '>'])
        }) else {
            continue;
        };
        let mut lhs = t[..eq].trim_end().to_string();
        // A pattern over several lines ends in a lone bracket: take it
        // whole, from its opening bracket's line, as one line.
        if let Some((open, close)) = [('(', ')'), ('[', ']'), ('{', '}')].into_iter().find(|&(_, c)| lhs == c.to_string()) {
            let mut depth = 0i32;
            let start = src[..=at].char_indices().rev().find(|&(_, c)| {
                depth += (c == close) as i32 - (c == open) as i32;
                depth == 0
            });
            if let Some((i, _)) = start {
                let from = src[..i].rfind('\n').map_or(0, |n| n + 1);
                lhs = src[from..=at].split_whitespace().collect::<Vec<_>>().join(" ");
            }
        }
        if lhs.starts_with("let ") {
            continue;
        }
        if (lhs.starts_with(['(', '[']) || lhs.ends_with('}')) && names_focus(&lhs) {
            hits.push(line.to_string());
        }
    }
    hits
}

#[test]
fn focus_written_only_by_set_focus() {
    let src = super::scan_tests::crate_source().replace("\r\n", "\n");
    // The one spaced write is `set_focus`'s (the field assigned, not compared).
    let pat = [".focus", " = "].concat();
    assert_eq!(src.matches(pat.as_str()).count(), 1);
    let at = src.find(pat.as_str()).unwrap();
    assert!(src[..at].rfind("fn set_focus").is_some_and(|f| at - f < 600));
    assert_eq!(stray_focus_writes(&src), Vec::<String>::new());
    // Every other spelling is caught (`FOCUS` keeps this text from matching).
    for case in [
        "x.FOCUS=y;",
        "std::mem::swap(&mut a, &mut s.FOCUS);",
        "(s.FOCUS, b) = (c, d);",
        "State { FOCUS, .. } = other;",
        "(\n    self.FOCUS,\n    other,\n) = pair;",
        "(\r\n    self.FOCUS,\r\n    other,\r\n) = pair;",
        "let f = &mut self.FOCUS;",
    ] {
        let case = case.replace("FOCUS", "focus");
        assert!(!stray_focus_writes(&case).is_empty(), "missed: {case:?}");
    }
}

#[test]
fn leave_never_ends_the_drawer() {
    // `leave` serves every intent alike: no early return, and no end of
    // the drawer's session from the window (the daemon's Close ends it,
    // a Keep keeps it).
    let src = super::scan_tests::crate_source().replace("\r\n", "\n");
    let start = src.find(&["fn leave(&mut self, event_loop: &ActiveEventLoop, ", "intent"].concat()).unwrap();
    let body = &src[start..start + src[start..].find("\n    }\n").unwrap()];
    for banned in ["attach_term", "request_quit", "return"] {
        assert!(!body.contains(banned), "`leave` contains `{banned}`");
    }
    // Every leave sets `should_exit` before it polls or exits, so
    // `about_to_wait` polls the acks.
    let set = body.find(&["self.should_exit = ", "true;"].concat()).expect("`leave` sets `should_exit`");
    assert!(body.find("request_redraw").is_some_and(|i| set < i), "{body}");
    assert!(body.find("self.finish_exit(").is_some_and(|i| set < i), "{body}");
}

#[test]
fn roi_paste_dismisses_the_prompt_first() {
    // An open quit prompt is dismissed (`set_focus`) before the agent
    // pane takes the ROI paste's bytes.
    let src = super::scan_tests::crate_source().replace("\r\n", "\n");
    let at = src.find(&["\"ROI {w}", "×{h} of {name}"].concat()).unwrap();
    let arm = &src[at.saturating_sub(4000)..at];
    let focus = arm.rfind(&["self.set_focus(", "PaneFocus::Llm);"].concat()).unwrap();
    let send = arm.rfind(&["self.send_pane_input(", "&bytes);"].concat()).unwrap();
    assert!(focus < send, "the ROI paste reaches the agent before the focus move dismisses the prompt");
}
