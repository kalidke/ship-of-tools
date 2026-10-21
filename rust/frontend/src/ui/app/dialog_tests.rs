use super::*;

/// The dialog's rows painted into a buffer the size of `area`, read back row by row.
fn painted(area: Rect, d: &CloseDialog) -> Vec<String> {
    let mut buf = ratatui::buffer::Buffer::empty(area);
    for (x, y, text, style) in &d.rows {
        buf.set_string(*x, *y, text, *style);
    }
    (area.y..area.y + area.height).map(|y| (area.x..area.x + area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

/// The text in `r`'s cells.
fn cells(rows: &[String], r: Rect) -> String {
    rows[r.y as usize].chars().skip(r.x as usize).take(r.width as usize).collect()
}

/// The panel sits in the middle of the window, within a cell each way, and holds the window's centre cell.
fn assert_centred(area: Rect, d: &CloseDialog) {
    let p = d.panel;
    let (left, right) = (p.x - area.x, area.x + area.width - p.x - p.width);
    let (top, bottom) = (p.y - area.y, area.y + area.height - p.y - p.height);
    assert!(left.abs_diff(right) <= 1 && top.abs_diff(bottom) <= 1, "{area:?}: the panel {p:?} is off centre");
    let centre = Position { x: area.x + area.width / 2, y: area.y + area.height / 2 };
    assert!(p.contains(centre), "{area:?}: the panel {p:?} misses the centre {centre:?}");
    for (x, y, text, _) in &d.rows {
        let row = Rect { x: *x, y: *y, width: text.chars().count() as u16, height: 1 };
        assert!(p.union(row) == p, "{area:?}: the row {text:?} leaves the panel {p:?}");
    }
}

/// The Ctrl+Q question is a panel in the middle of the window, at a full-screen, a laptop and an 80x24 window:
/// its title, its question, the lease notice when there is one, its two answers on one row with the selected one
/// bracketed, and its keys.
#[test]
fn the_close_question_sits_in_the_middle_of_the_window_with_its_answers() {
    let notice = "keep not done: this computer's backend did not answer the window lease in time, so the window \
            stayed open and the sessions run on; Ctrl+Q then Yes tries again";
    for area in [Rect::new(0, 0, 200, 56), Rect::new(0, 0, 120, 36), Rect::new(0, 0, 80, 24)] {
        for (keep, notice) in [(false, None), (true, None), (true, Some(notice))] {
            let d = close_dialog(area, CloseDialogText::Ask { keep, notice });
            assert_centred(area, &d);
            let rows = painted(area, &d);
            let all = rows.iter().map(|r| r.trim()).collect::<Vec<_>>().join(" ");
            for want in ["Close the window", QUIT_QUESTION, "Tab switches", "Enter confirms", "any other key cancels"]
                .into_iter().chain(notice) {
                let words: Vec<&str> = want.split_whitespace().collect();
                assert!(all.contains(&words.join(" ")), "{area:?}: {want:?} is not in {rows:#?}");
            }
            let [no, yes] = d.answers.expect("the question has answers");
            assert_eq!(no.y, yes.y, "one row of answers");
            let want = if keep { ("  No  ", "[ Yes ]") } else { ("[ No ]", "  Yes  ") };
            assert_eq!((cells(&rows, no), cells(&rows, yes)), (want.0.to_string(), want.1.to_string()), "{area:?}");
            let mid = d.panel.x + d.panel.width / 2;
            assert!(no.x + no.width <= mid && yes.x >= mid, "{area:?}: the answers straddle the middle");
        }
    }
}

/// While the window waits on the close, and when the close's outcome shows, the dialog is the same panel in the
/// middle of the window, saying what it waits for, then how it ended, whole; it has no answers.
#[test]
fn the_close_wait_and_its_outcome_sit_in_the_middle_of_the_window() {
    let lines = ["closing: waiting for this computer's backend; it ends the sessions only when no other window is open on this computer",
        "closing: waiting for this computer's backend to keep the sessions running; killing the window before it answers ends them when no other window is open",
        "2 sessions could not be ended and are still running\nclosing: this computer's backend did not confirm the sessions ended"];
    for area in [Rect::new(0, 0, 200, 56), Rect::new(0, 0, 120, 36), Rect::new(0, 0, 80, 24)] {
        for line in lines {
            let d = close_dialog(area, CloseDialogText::Leave(line));
            assert_centred(area, &d);
            assert!(d.answers.is_none());
            let rows = painted(area, &d);
            let all = rows.iter().map(|r| r.trim()).collect::<Vec<_>>().join(" ");
            assert!(all.contains("Closing the window"), "{rows:#?}");
            for l in line.lines() {
                assert!(all.contains(l), "{area:?}: {l:?} is not whole in {rows:#?}");
            }
        }
    }
}

/// The question shows while the quit prompt is open, else a leave's line does; no other prompt and no leave
/// shows none.
#[test]
fn the_dialog_shows_the_question_or_the_leave() {
    let quit = NavPrompt::ConfirmQuit { keep: true };
    let other = NavPrompt::ConfirmDelete { node_id: "files:a".into(), label: "a".into() };
    assert_eq!(close_dialog_text(Some(&quit), Some("n"), Some("l"), None, None),
        Some(CloseDialogText::Ask { keep: true, notice: Some("n") }));
    assert_eq!(close_dialog_text(Some(&other), None, Some("l"), None, None), Some(CloseDialogText::Leave("l")));
    assert_eq!(close_dialog_text(None, Some("n"), Some("l"), None, None), Some(CloseDialogText::Leave("l")));
    assert_eq!(close_dialog_text(Some(&other), Some("n"), None, None, None), None);
    assert_eq!(close_dialog_text(None, Some("n"), None, None, None), None);
}

/// The quit hint is a panel in the middle of the window, as the question is, with no answers to press; any real
/// dialog text outranks it.
#[test]
fn the_quit_hint_sits_in_the_middle_of_the_window_and_gives_way_to_the_question() {
    let hint = crate::ui::input::global_keys::quit_hint("Ctrl+Q");
    for area in [Rect::new(0, 0, 200, 56), Rect::new(0, 0, 120, 36), Rect::new(0, 0, 80, 24)] {
        let d = close_dialog(area, CloseDialogText::Hint(&hint));
        assert_centred(area, &d);
        assert!(d.answers.is_none(), "the hint has nothing to answer");
        let rows = painted(area, &d);
        let all = rows.iter().map(|r| r.trim()).collect::<Vec<_>>().join(" ");
        let words: Vec<&str> = hint.split_whitespace().collect();
        assert!(all.contains(&words.join(" ")), "{area:?}: the hint is not whole in {rows:#?}");
    }
    let quit = NavPrompt::ConfirmQuit { keep: false };
    assert_eq!(close_dialog_text(None, None, None, None, Some("h")), Some(CloseDialogText::Hint("h")));
    assert_eq!(close_dialog_text(Some(&quit), None, None, None, Some("h")),
        Some(CloseDialogText::Ask { keep: false, notice: None }));
    assert_eq!(close_dialog_text(None, None, Some("l"), None, Some("h")), Some(CloseDialogText::Leave("l")));
    assert_eq!(close_dialog_text(None, None, None, None, None), None);
}

/// A left press on an answer gives that answer; a press anywhere else, or any other button, cancels.
#[test]
fn a_press_answers_the_question_or_cancels_it() {
    let area = Rect::new(0, 0, 120, 36);
    for keep in [false, true] {
        let answers = close_dialog(area, CloseDialogText::Ask { keep, notice: None }).answers;
        let [no, yes] = answers.unwrap();
        for (at, want) in [
            (Some((no.x, no.y)), QuitPromptStep::Leave(LeaveIntent::Close)),
            (Some((no.x + no.width - 1, no.y)), QuitPromptStep::Leave(LeaveIntent::Close)),
            (Some((yes.x + 3, yes.y)), QuitPromptStep::Leave(LeaveIntent::Keep)),
            (Some((no.x + no.width, no.y)), QuitPromptStep::Cancel),
            (Some((yes.x, yes.y + 1)), QuitPromptStep::Cancel),
            (Some((0, 0)), QuitPromptStep::Cancel),
            (None, QuitPromptStep::Cancel),
        ] {
            assert_eq!(quit_prompt_click(answers, at), want, "keep {keep}, press at {at:?}");
        }
    }
    assert_eq!(quit_prompt_click(None, Some((1, 1))), QuitPromptStep::Cancel);
}

/// A relaunch that stayed open shows its reason in the same centred panel, whole, with no answers, only while
/// it is flagged and a notice stands; the question and a leave's line come before it; any key ends it, once.
#[test]
fn a_relaunch_that_stays_says_why_in_the_dialog_until_a_key() {
    let notice = "relaunch not done: this computer's backend did not answer the window lease in time, so the \
            window stayed open; ask for the relaunch again";
    // The panel shows the stay's own text, never a standing's cause under "The window stayed open".
    let cause = "closing will not end sessions: this computer's backend could not verify this window";
    assert_eq!(close_dialog_text(None, Some(cause), None, Some(notice), None), Some(CloseDialogText::Stayed(notice)));
    assert_eq!(close_dialog_text(None, Some(cause), None, None, None), None);
    let quit = NavPrompt::ConfirmQuit { keep: false };
    assert_eq!(close_dialog_text(Some(&quit), Some(notice), None, Some("s"), None),
        Some(CloseDialogText::Ask { keep: false, notice: Some(notice) }));
    assert_eq!(close_dialog_text(None, Some(notice), Some("l"), Some("s"), None), Some(CloseDialogText::Leave("l")));
    for area in [Rect::new(0, 0, 200, 56), Rect::new(0, 0, 120, 36), Rect::new(0, 0, 80, 24)] {
        let d = close_dialog(area, CloseDialogText::Stayed(notice));
        assert_centred(area, &d);
        assert!(d.answers.is_none());
        let rows = painted(area, &d);
        let all = rows.iter().map(|r| r.trim()).collect::<Vec<_>>().join(" ");
        for want in ["The window stayed open", notice, STAYED_KEYS] {
            let words: Vec<&str> = want.split_whitespace().collect();
            assert!(all.contains(&words.join(" ")), "{area:?}: {want:?} is not in {rows:#?}");
        }
    }
    let mut shown = true;
    assert_eq!(dismiss_stay(&mut shown, false), Some(true), "a key ends it and is eaten");
    assert!(!shown);
    assert_eq!(dismiss_stay(&mut shown, false), None, "and only once");
    // The agent, the terminal and the other text panes keep the key the user was typing.
    shown = true;
    assert_eq!(dismiss_stay(&mut shown, true), Some(false));
    assert!(!shown, "it still ends the dialog");
    // A press ends it too and reaches no pane.
    shown = true;
    assert!(dismiss_stay_press(&mut shown) && !shown);
    assert!(!dismiss_stay_press(&mut shown), "and only once");
}
