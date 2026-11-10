//! The chat draft: what the person typed, one per canonical row, with the destination it was begun in. It never escapes
//! its text (chat's door does, B5l); it holds only what `post_text_fits` admits, so the post reads back whole.

use std::collections::HashMap;

use sot_protocol::chat::{post_text_fits, Ts};
use sot_protocol::ops::ChatTarget;

use crate::ui::chrome::{cell_lines, wrap_status};
use crate::ui::ResultRowIdentity;

/// The draft rows of the compose block; the row naming where the draft goes (the channel, or the thread) follows them.
const DRAFT_ROWS: usize = 3;

pub(in crate::ui) enum Edit<'a> {
    /// Typed or pasted text.
    Text(&'a str),
    Space,
    Backspace,
    Newline,
}

/// What was typed and where it goes. The destination, the channel and the thread when one was open, is fixed at the first character.
pub(super) struct Draft {
    pub text: String,
    pub dest: ChatTarget,
    pub thread: Option<Ts>,
}

#[derive(Default)]
pub(super) struct Compose {
    drafts: HashMap<ResultRowIdentity, Draft>,
}

/// Typed or pasted text as the draft keeps it: CR LF and CR are LF, and every control character (C0, DEL and C1) but LF
/// and tab is dropped, which is exactly what the drawer cannot show (it draws each as U+FFFD, and each C0 costs six
/// bytes of the record's JSON).
fn clean(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

impl Compose {
    /// Applies `edit` to `row`'s draft, begun in `dest` (the channel now watched, with the thread open, if any) when there
    /// is none. A refusal is the status line's words; the draft is then as it was.
    pub(super) fn edit(&mut self, row: &ResultRowIdentity, dest: Option<(&ChatTarget, Option<&Ts>)>, edit: Edit) -> Result<(), &'static str> {
        let add = match edit {
            Edit::Text(s) => clean(s),
            Edit::Space => " ".into(),
            Edit::Newline => "\n".into(),
            Edit::Backspace => {
                if let Some(d) = self.drafts.get_mut(row) {
                    d.text.pop();
                    if d.text.is_empty() {
                        self.drafts.remove(row);
                    }
                }
                return Ok(());
            }
        };
        if add.is_empty() {
            return Ok(());
        }
        let Some((dest, thread)) = self.drafts.get(row).map(|d| (&d.dest, d.thread.as_ref())).or(dest) else {
            return Err("no channel to write to");
        };
        let text = self.drafts.get(row).map_or_else(String::new, |d| d.text.clone()) + &add;
        if !post_text_fits(&text) {
            return Err("draft is full: nothing added");
        }
        let (dest, thread) = (dest.clone(), thread.cloned());
        self.drafts.insert(row.clone(), Draft { text, dest, thread });
        Ok(())
    }

    /// A post was made: the row's draft is done.
    pub(super) fn clear(&mut self, row: &ResultRowIdentity) {
        self.drafts.remove(row);
    }

    /// Rows a listing retired lose their drafts.
    pub(super) fn retire(&mut self, rows: &[ResultRowIdentity]) {
        self.drafts.retain(|row, _| !rows.contains(row));
    }

    pub(super) fn draft(&self, row: &ResultRowIdentity) -> Option<&Draft> {
        self.drafts.get(row)
    }
}

/// What the compose rows are made of: the draft's lines (the cursor block ends the last while the drawer has focus) and
/// where it goes (`to <channel>`, or `reply in thread of HH:MM`).
pub(in crate::ui) struct ComposeText {
    pub lines: Vec<String>,
    pub to: String,
}

impl ComposeText {
    pub(super) fn of(draft: Option<&str>, to: String, focused: bool) -> Self {
        let text = draft.unwrap_or("");
        let mut lines = cell_lines(text);
        if lines.is_empty() || text.ends_with('\n') {
            lines.push(String::new());
        }
        if focused {
            lines.last_mut().expect("a line").push('█');
        }
        ComposeText { lines, to }
    }
}

/// The compose rows for a drawer `width` cells wide and `height` rows tall: `> ` and the draft's last three rows, each
/// line wrapped to the width, then its destination with how many rows are not shown. A drawer too short for three keeps
/// one row fewer than its height, so the destination is the last row dropped and the count is true at every height.
pub(in crate::ui) fn compose_rows(text: &ComposeText, width: usize, height: usize) -> Vec<String> {
    let inner = width.saturating_sub(2).max(1);
    let wrapped: Vec<String> = text.lines.iter().flat_map(|l| wrap_status(l, inner, inner)).collect();
    let hidden = wrapped.len().saturating_sub(DRAFT_ROWS.min(height.saturating_sub(1)));
    let mut out: Vec<String> = wrapped[hidden..].iter().enumerate().map(|(i, l)| format!("{}{l}", if i == 0 { "> " } else { "  " })).collect();
    out.push(match hidden {
        0 => text.to.clone(),
        1 => format!("{} · 1 line not shown", text.to),
        n => format!("{} · {n} lines not shown", text.to),
    });
    out.split_off(out.len().saturating_sub(height))
}
