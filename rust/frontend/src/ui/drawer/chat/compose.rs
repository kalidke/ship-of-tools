//! The chat draft: what the person typed, one per canonical row, with the destination it was begun in. It never escapes
//! its text (chat's door does, B5l); it holds only what `post_text_fits` admits, so the post reads back whole.

#![cfg_attr(not(test), allow(dead_code, reason = "the draft's wiring (B5k) uses it"))]

use std::collections::HashMap;

use sot_protocol::chat::post_text_fits;
use sot_protocol::ops::ChatTarget;

use crate::ui::ResultRowIdentity;

pub(in crate::ui) enum Edit<'a> {
    /// Typed or pasted text.
    Text(&'a str),
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

/// Typed or pasted text as the draft keeps it: CR LF and CR are LF, and every control character (C0, DEL and C1) but LF
/// and tab is dropped, which is exactly what the drawer cannot show (it draws each as U+FFFD, and each C0 costs six
/// bytes of the record's JSON).
fn clean(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

impl Compose {
    /// Applies `edit` to `row`'s draft, begun in `dest` (the channel now watched) when there is none. A refusal is the
    /// status line's words; the draft is then as it was.
    pub(super) fn edit(&mut self, row: &ResultRowIdentity, dest: Option<&ChatTarget>, edit: Edit) -> Result<(), &'static str> {
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
