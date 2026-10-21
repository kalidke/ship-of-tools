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
    leave_line: Option<&'a str>, stayed: Option<&'a str>, hint: Option<&'a str>) -> Option<CloseDialogText<'a>> {
    match (prompt, leave_line) {
        (Some(NavPrompt::ConfirmQuit { keep }), _) => Some(CloseDialogText::Ask { keep: *keep, notice }),
        (_, Some(line)) => Some(CloseDialogText::Leave(line)),
        _ => stayed.map(CloseDialogText::Stayed).or_else(|| hint.map(CloseDialogText::Hint)),
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
/// eaten: it is, except where the focused pane consumes typed text, since the dialog appears on its own and the
/// user may be typing into the agent, and that character is theirs.
pub(in crate::ui) fn dismiss_stay(relaunch_stayed: &mut bool, consumes_text: bool) -> Option<bool> {
    std::mem::take(relaunch_stayed).then_some(!consumes_text)
}

/// A press while the dialog shows ends it and reaches no pane. True when it was showing.
pub(in crate::ui) fn dismiss_stay_press(relaunch_stayed: &mut bool) -> bool {
    std::mem::take(relaunch_stayed)
}

impl State {
    /// The close dialog this frame shows, on the chrome's grid (`close_dialog_text`).
    pub(in crate::ui) fn close_dialog(&self) -> Option<CloseDialog> {
        let size = self.terminal.size().ok()?;
        let line = self.leaving.as_ref().and_then(crate::lease::Leaving::line);
        let text = close_dialog_text(self.nav_prompt.as_ref(), self.leases.notice(), line.as_deref(),
            self.relaunch_stayed.then(|| self.leases.stay_notice()).flatten(), self.quit_hint.as_ref().map(|(h, _)| h.as_str()))?;
        Some(close_dialog(Rect::new(0, 0, size.width, size.height), text))
    }
}

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod tests;
