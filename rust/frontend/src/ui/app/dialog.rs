//! The close dialog: the Ctrl+Q question, and a close's wait and outcome, as a panel of cells centred on the window
//! and drawn over every pane (`prepare_overlays`), so neither is only a line at the foot of the nav pane. A page's
//! reply offer (the reply bar) is drawn as the same panel, under the close dialog.

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
    /// A reply a page offered, for the person's word: who it is for, the exact line, and what the keys do.
    Reply { to: &'a str, line: &'a str, keys: &'a str },
}

/// The close dialog in cells: the panel its backing covers; each row's column, row, text and style; while it asks,
/// its answers' cells, No then Yes, for a press to hit; and whether the area was too small to draw every row whole.
#[derive(Debug)]
pub(in crate::ui) struct CloseDialog {
    pub(in crate::ui) panel: Rect,
    pub(in crate::ui) rows: Vec<(u16, u16, String, Style)>,
    pub(in crate::ui) answers: Option<[Rect; 2]>,
    pub(in crate::ui) clipped: bool,
}

/// What a key does to a stayed relaunch's dialog, said in the dialog.
const STAYED_KEYS: &str = "any key closes this";

/// The widest the dialog's text runs, in cells.
const TEXT_MAX: u16 = 60;

/// The narrowest text column the dialog counts as drawn whole: in a narrower one a line wraps a few characters to a
/// row and a wrap drops its spaces, so what shows is no longer the line.
const TEXT_MIN: u16 = 20;

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
        CloseDialogText::Reply { to, line, keys } => {
            body.extend(wrapped(to, attention));
            body.push(blank.clone());
            body.extend(wrapped(line, style(Color::White, false)));
            body.push(blank.clone());
            body.extend(wrapped(keys, style(Color::DarkGray, false)));
            ("Reply from a page", None)
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
    let rows: Vec<(u16, u16, String, Style)> = rows.into_iter().filter(|(_, y, (t, _))| *y < foot && !t.is_empty()).map(|(x, y, (t, s))| (x, y, t, s))
        .collect();
    // Cut off by the window's foot or its side, or too narrow to read whole: a row below the foot is not drawn, and
    // one that runs past the right edge is cut by the glyph layer.
    let clipped = ph > area.height || area.width.saturating_sub(4) < TEXT_MIN
        || rows.iter().any(|(x, _, t, _)| usize::from(*x) + unicode_width::UnicodeWidthStr::width(t.as_str())
            > usize::from(area.x + area.width));
    CloseDialog { panel, rows, answers, clipped }
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

impl State {
    /// `f` of the close dialog's text this frame shows, if one does (`close_dialog_text`).
    fn with_close_dialog_text<R>(&self, f: impl FnOnce(CloseDialogText<'_>) -> R) -> Option<R> {
        let line = self.leaving.as_ref().and_then(crate::lease::Leaving::line);
        let text = close_dialog_text(self.focus_prompts.prompt(), self.leases.notice(), line.as_deref(),
            self.focus_prompts.stayed().then(|| self.leases.stay_notice()).flatten(), self.focus_prompts.hint())?;
        Some(f(text))
    }

    /// The close dialog this frame shows, on the chrome's grid (`close_dialog_text`).
    pub(in crate::ui) fn close_dialog(&self) -> Option<CloseDialog> {
        let size = self.canvas.grid_size().ok()?;
        self.with_close_dialog_text(|text| close_dialog(Rect::new(0, 0, size.width, size.height), text))
    }

    /// Whether a close dialog is drawn now, which is whether it takes every key.
    pub(in crate::ui) fn close_dialog_open(&self) -> bool {
        self.with_close_dialog_text(|_| ()).is_some()
    }
}

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod tests;
