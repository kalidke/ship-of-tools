//! The chat draft: what the person typed, one per canonical row, with the destination it was begun in. It never escapes
//! its text (chat's door does, B5l); it holds only what `post_text_fits` admits, so the post reads back whole.

use std::collections::HashMap;

use sot_protocol::chat::post_text_fits;
use sot_protocol::ops::ChatTarget;

use crate::ui::chrome::cell_lines;
use crate::ui::ResultRowIdentity;

/// The compose block's draft rows; the destination row follows them.
const DRAFT_ROWS: usize = 3;

pub(in crate::ui) enum Edit<'a> {
    Typed(&'a str),
    Paste(&'a str),
    Space,
    Backspace,
    Newline,
}

/// What was typed and where it goes. The destination is fixed at the first character; until threads (B5p) it is the channel.
pub(super) struct Draft {
    pub text: String,
    pub dest: ChatTarget,
}

#[derive(Default)]
pub(super) struct Compose {
    drafts: HashMap<ResultRowIdentity, Draft>,
}

/// Typed or pasted text as the draft keeps it: CR LF and CR are LF, and every other C0 control but tab is dropped (the
/// drawer would show it as U+FFFD, and each costs six bytes of the record's JSON).
fn clean(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").chars().filter(|c| !c.is_ascii_control() || matches!(c, '\n' | '\t')).collect()
}

impl Compose {
    /// Applies `edit` to `row`'s draft, begun in `dest` (the channel now watched) when there is none. A refusal is the
    /// status line's words; the draft is then as it was.
    pub(super) fn edit(&mut self, row: &ResultRowIdentity, dest: Option<&ChatTarget>, edit: Edit) -> Result<(), &'static str> {
        let add = match edit {
            Edit::Typed(s) | Edit::Paste(s) => clean(s),
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
        let Some(dest) = self.drafts.get(row).map(|d| &d.dest).or(dest) else { return Err("no channel to write to") };
        let text = self.drafts.get(row).map_or_else(String::new, |d| d.text.clone()) + &add;
        if !post_text_fits(&text) {
            return Err("draft is full: nothing added");
        }
        let dest = dest.clone();
        self.drafts.insert(row.clone(), Draft { text, dest });
        Ok(())
    }

    /// Rows a listing retired lose their drafts.
    pub(super) fn retire(&mut self, rows: &[ResultRowIdentity]) {
        self.drafts.retain(|row, _| !rows.contains(row));
    }

    pub(super) fn draft(&self, row: &ResultRowIdentity) -> Option<&Draft> {
        self.drafts.get(row)
    }
}

/// The compose rows at the body's foot: `> ` and the draft's last lines (the cursor block ends them when `focused`), then
/// `to <channel>`, which also says how many lines are not shown.
pub(super) fn rows(draft: Option<&str>, channel: &str, focused: bool) -> Vec<String> {
    let text = draft.unwrap_or("");
    let mut lines = cell_lines(text);
    if lines.is_empty() || text.ends_with('\n') {
        lines.push(String::new());
    }
    if focused {
        lines.last_mut().expect("a line").push('█');
    }
    let hidden = lines.len().saturating_sub(DRAFT_ROWS);
    let mut out: Vec<String> = lines.split_off(hidden).into_iter().enumerate().map(|(i, l)| format!("{}{l}", if i == 0 { "> " } else { "  " })).collect();
    out.push(match hidden {
        0 => format!("to {channel}"),
        1 => format!("to {channel} · 1 line not shown"),
        n => format!("to {channel} · {n} lines not shown"),
    });
    out
}
