//! The close dialog: the Ctrl+Q question, and a close's wait and outcome, as a panel of cells centred on the window
//! and drawn over every pane (`prepare_overlays`), so neither is only a line at the foot of the nav pane.

use super::*;
use ratatui::layout::{Position, Rect};

/// What the close dialog shows: the Ctrl+Q question, its selected answer and the lease notice, which says why closing
/// will not end the sessions or why a keep was not done; or a leave's line (`Leaving::line`), what it waits for and
/// then how it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum CloseDialogText<'a> {
    Ask { keep: bool, notice: Option<&'a str> },
    Leave(&'a str),
    /// A relaunch that stayed open: the lease notice saying why, until any key.
    Stayed(&'a str),
    /// The quit chord went to the program: a hint that takes no keys and ends on its own (`QUIT_HINT_HOLD`).
    Hint(&'a str),
}

/// The close dialog in cells: the panel its backing covers; each row's column, row, text and style; and, while it
/// asks, its answers' cells, No then Yes, for a press to hit.
#[derive(Debug)]
pub(in crate::ui) struct CloseDialog {
    pub(in crate::ui) panel: Rect,
    pub(in crate::ui) rows: Vec<(u16, u16, String, Style)>,
    pub(in crate::ui) answers: Option<[Rect; 2]>,
}

/// What a key does to a stayed relaunch's dialog, said in the dialog.
const STAYED_KEYS: &str = "any key closes this";

/// The widest the dialog's text runs, in cells.
const TEXT_MAX: u16 = 60;

/// How long the quit hint shows.
pub(in crate::ui) const QUIT_HINT_HOLD: std::time::Duration = std::time::Duration::from_secs(4);

/// The dialog an open quit prompt, a leave with a line, a relaunch that stayed or the quit hint shows, in that order.
/// Every other prompt is the nav pane's own.
pub(in crate::ui) fn close_dialog_text<'a>(prompt: Option<&NavPrompt>, notice: Option<&'a str>,
    leave_line: Option<&'a str>, relaunch_stayed: bool, hint: Option<&'a str>) -> Option<CloseDialogText<'a>> {
    match (prompt, leave_line) {
        (Some(NavPrompt::ConfirmQuit { keep }), _) => Some(CloseDialogText::Ask { keep: *keep, notice }),
        (_, Some(line)) => Some(CloseDialogText::Leave(line)),
        _ if relaunch_stayed && notice.is_some() => notice.map(CloseDialogText::Stayed),
        _ => hint.map(CloseDialogText::Hint),
    }
}

/// The dialog centred on `area`: a top rule holding its title, its text wrapped at `TEXT_MAX` cells or the area's
/// width less four, two cells in from the panel's sides, and a bottom rule. The question's answers sit centred on
/// their own row, the selected one bracketed. A row past the area's foot is not drawn.
pub(in crate::ui) fn close_dialog(area: Rect, text: CloseDialogText<'_>) -> CloseDialog {
    let w = area.width.saturating_sub(4).clamp(1, TEXT_MAX);
    let style = |c: Color, bold: bool| {
        let s = Style::default().fg(c);
        if bold { s.add_modifier(Modifier::BOLD) } else { s }
    };
    let attention = style(Color::Yellow, true);
    let wrapped = |t: &str, s: Style| -> Vec<(String, Style)> {
        cell_lines(t).iter().flat_map(|l| wrap_status(l, w as usize, w as usize)).map(|r| (r, s)).collect()
    };
    let blank = (String::new(), Style::default());
    let mut body = vec![blank.clone()];
    let (title, ask) = match text {
        CloseDialogText::Ask { keep, notice } => {
            body.extend(wrapped(QUIT_QUESTION, attention));
            if let Some(n) = notice {
                body.push(blank.clone());
                body.extend(wrapped(n, style(Color::LightGreen, false)));
            }
            body.push(blank.clone());
            let at = body.len();
            body.extend([blank.clone(), blank.clone()]);
            body.extend(wrapped(QUIT_KEYS, style(Color::DarkGray, false)));
            ("Close the window", Some((at as u16, keep)))
        }
        CloseDialogText::Leave(line) => {
            body.extend(wrapped(line, attention));
            ("Closing the window", None)
        }
        CloseDialogText::Stayed(notice) => {
            body.extend(wrapped(notice, attention));
            body.push(blank.clone());
            body.extend(wrapped(STAYED_KEYS, style(Color::DarkGray, false)));
            ("The window stayed open", None)
        }
        CloseDialogText::Hint(hint) => {
            body.extend(wrapped(hint, attention));
            ("Quit", None)
        }
    };
    body.push(blank);
    let (pw, ph) = ((w + 4).min(area.width), body.len() as u16 + 2);
    let panel = Rect {
        x: area.x + (area.width - pw) / 2,
        y: area.y + area.height.saturating_sub(ph) / 2,
        width: pw,
        height: ph.min(area.height),
    };
    let rule = |head: String| (head.chars().chain(std::iter::repeat('\u{2500}')).take(pw as usize).collect(), attention);
    let mut rows = vec![(panel.x, panel.y, rule(format!("\u{2500}\u{2500} {title} ")))];
    rows.extend(body.into_iter().enumerate().map(|(i, r)| (panel.x + 2, panel.y + 1 + i as u16, r)));
    rows.push((panel.x, panel.y + ph - 1, rule(String::new())));
    let answers = ask.map(|(at, keep)| {
        let pick = |on: bool, label: &str| (if on { format!("[ {label} ]") } else { format!("  {label}  ") },
            if on { attention } else { style(Color::Gray, false) });
        let y = panel.y + 1 + at;
        let no = Rect { x: (panel.x + pw / 2).saturating_sub(8), y, width: 6, height: 1 };
        let yes = Rect { x: no.x + 10, width: 7, ..no };
        rows.push((no.x, y, pick(!keep, "No")));
        rows.push((yes.x, y, pick(keep, "Yes")));
        [no, yes]
    });
    let foot = area.y + area.height;
    let rows = rows.into_iter().filter(|(_, y, (t, _))| *y < foot && !t.is_empty()).map(|(x, y, (t, s))| (x, y, t, s))
        .collect();
    CloseDialog { panel, rows, answers }
}

/// What a press does while the question is open: a left press on an answer gives that answer; any other press
/// cancels, as any other key does (`quit_prompt_step`).
pub(in crate::ui) fn quit_prompt_click(answers: Option<[Rect; 2]>, left_at: Option<(u16, u16)>) -> QuitPromptStep {
    let on = |r: Rect| left_at.is_some_and(|(x, y)| r.contains(Position { x, y }));
    let step = match answers {
        Some([no, _]) if on(no) => QuitPromptStep::Leave(LeaveIntent::Close),
        Some([_, yes]) if on(yes) => QuitPromptStep::Leave(LeaveIntent::Keep),
        _ => QuitPromptStep::Cancel,
    };
    tracing::info!(?step, "quit prompt: press");
    step
}

/// A key while a stayed relaunch's dialog shows ends it. `None` when it was not showing; else whether the key is
/// eaten: it is, except where the focused pane consumes typed text, since a relaunch ends the dialog on its own and the
/// user may be typing into the agent, and that character is theirs.
pub(in crate::ui) fn dismiss_stay(relaunch_stayed: &mut bool, consumes_text: bool) -> Option<bool> {
    std::mem::take(relaunch_stayed).then_some(!consumes_text)
}

impl State {
    /// The close dialog this frame shows, on the chrome's grid (`close_dialog_text`).
    pub(in crate::ui) fn close_dialog(&self) -> Option<CloseDialog> {
        let size = self.terminal.size().ok()?;
        let line = self.leaving.as_ref().and_then(crate::lease::Leaving::line);
        let text = close_dialog_text(self.nav_prompt.as_ref(), self.leases.notice(), line.as_deref(),
            self.relaunch_stayed, self.quit_hint.as_ref().map(|(h, _)| h.as_str()))?;
        Some(close_dialog(Rect::new(0, 0, size.width, size.height), text))
    }
}

#[cfg(test)]
mod tests {
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
        let lines = ["closing: waiting for this computer's backend to end the sessions",
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
        assert_eq!(close_dialog_text(Some(&quit), Some("n"), Some("l"), false, None),
            Some(CloseDialogText::Ask { keep: true, notice: Some("n") }));
        assert_eq!(close_dialog_text(Some(&other), None, Some("l"), false, None), Some(CloseDialogText::Leave("l")));
        assert_eq!(close_dialog_text(None, Some("n"), Some("l"), false, None), Some(CloseDialogText::Leave("l")));
        assert_eq!(close_dialog_text(Some(&other), Some("n"), None, false, None), None);
        assert_eq!(close_dialog_text(None, Some("n"), None, false, None), None);
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
        assert_eq!(close_dialog_text(None, None, None, false, Some("h")), Some(CloseDialogText::Hint("h")));
        assert_eq!(close_dialog_text(Some(&quit), None, None, false, Some("h")),
            Some(CloseDialogText::Ask { keep: false, notice: None }));
        assert_eq!(close_dialog_text(None, None, Some("l"), false, Some("h")), Some(CloseDialogText::Leave("l")));
        assert_eq!(close_dialog_text(None, None, None, false, None), None);
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
        assert_eq!(close_dialog_text(None, Some(notice), None, true, None), Some(CloseDialogText::Stayed(notice)));
        assert_eq!(close_dialog_text(None, Some(notice), None, false, None), None);
        assert_eq!(close_dialog_text(None, None, None, true, None), None);
        let quit = NavPrompt::ConfirmQuit { keep: false };
        assert_eq!(close_dialog_text(Some(&quit), Some(notice), None, true, None),
            Some(CloseDialogText::Ask { keep: false, notice: Some(notice) }));
        assert_eq!(close_dialog_text(None, Some(notice), Some("l"), true, None), Some(CloseDialogText::Leave("l")));
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
    }
}
