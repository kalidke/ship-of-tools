//! The pure half of sharing a window's selection with a row's agent: the check that it is the file's own text, and
//! its rendering as the hook's output. No I/O; the handler that reads the file, publishes and answers is unbuilt.

use sot_protocol::ops::SelectedText;
use sot_protocol::selection_digest;

/// The most UTF-16 code units of the range's text the agent is given.
const TEXT_CUT: usize = 2000;
/// The most UTF-16 code units of the whole `additionalContext` (Claude Code moves one over 10,000 into a file).
const CONTEXT_CAP: usize = 9_000;
/// The most UTF-16 code units of the path inside `additionalContext`.
const PATH_CAP: usize = 4096;
/// The most UTF-16 code units of the `systemMessage`.
const MESSAGE_CAP: usize = 200;

/// A range the file's own text matched: its text and the 1-based inclusive lines it covers.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code, reason = "the selection handler (A3) is the first caller"))]
pub(crate) struct FileText {
    pub(crate) text: String,
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
}

/// The file's own text at the window's range, or `None` (`not_file_text`). `bytes` is a prefix of the file that may
/// end mid-character after the range; a range reaching past it is refused. Lines are LF-ended with a CR before the LF
/// dropped, as the preview shows them; a lone CR, U+2028, U+2029 or U+0085 before the range's end is refused (the
/// preview breaks a line there, the file does not), and anything after it, or bytes that are not UTF-8 after it, are
/// not read. A position outside its line or inside a character, and a `len` or `digest` that differs from the file's
/// text, are refused.
#[cfg_attr(not(test), allow(dead_code, reason = "the selection handler (A3) is the first caller"))]
pub(crate) fn check_selection(bytes: &[u8], sel: &SelectedText) -> Option<FileText> {
    let valid = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&bytes[..e.valid_up_to()]).ok()?,
    };
    let (sl, sb) = (sel.start.line as usize, sel.start.byte as usize);
    let (el, eb) = (sel.end.line as usize, sel.end.byte as usize);
    if (sl, sb) >= (el, eb) {
        return None;
    }
    let mut lines: Vec<&str> = Vec::new();
    let (mut off, mut end_off) = (0, 0);
    for (i, raw) in valid.split('\n').enumerate() {
        let content = raw.strip_suffix('\r').unwrap_or(raw);
        if i == el {
            if eb > content.len() || !content.is_char_boundary(eb) {
                return None;
            }
            end_off = off + eb;
        }
        lines.push(content);
        if i == el {
            break;
        }
        off += raw.len() + 1;
    }
    if lines.len() <= el {
        return None;
    }
    let before_end = &valid[..end_off];
    for (i, c) in before_end.char_indices() {
        let bad = match c {
            '\r' => valid.as_bytes().get(i + 1) != Some(&b'\n'),
            '\u{85}' | '\u{2028}' | '\u{2029}' => true,
            _ => false,
        };
        if bad {
            return None;
        }
    }
    let first = lines[sl];
    if sb > first.len() || !first.is_char_boundary(sb) {
        return None;
    }
    let text = if sl == el {
        first[sb..eb].to_string()
    } else {
        let mut t = first[sb..].to_string();
        for l in &lines[sl + 1..el] {
            t.push('\n');
            t.push_str(l);
        }
        t.push('\n');
        t.push_str(&lines[el][..eb]);
        t
    };
    if u32::try_from(text.len()).ok()? != sel.len || selection_digest(&text) != sel.digest {
        return None;
    }
    Some(FileText { text, first_line: sl + 1, last_line: if eb == 0 { el } else { el + 1 } })
}

fn units(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The longest prefix of `s` of at most `max` UTF-16 code units, never splitting a character.
fn head(s: &str, max: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += c.len_utf16();
        if used > max {
            return &s[..i];
        }
    }
    s
}

/// `s` if it fits in `max` UTF-16 code units, else `...` and the longest tail that fits with it.
fn left_cut(s: &str, max: usize) -> String {
    if units(s) <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut used = 0;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        used += c.len_utf16();
        if used > keep {
            break;
        }
        start = i;
    }
    format!("...{}", &s[start..])
}

/// The one JSON object the selection hook prints: exactly `hookSpecificOutput` and `systemMessage`. `path` is the
/// file's absolute path, `rel` its path in the project. Every cap counts UTF-16 code units (Claude Code's `length`,
/// never smaller than a character count): the range's text is cut at 2000 with its whole size named, the path in the
/// context at 4096, the context is at most 9,000 and the message at most 200, whatever the paths.
#[cfg_attr(not(test), allow(dead_code, reason = "the selection handler (A3) is the first caller"))]
pub(crate) fn render_selection(f: &FileText, path: &str, rel: &str) -> String {
    let (a, b) = (f.first_line, f.last_line);
    let lead = format!("The user selected lines {a} to {b} of {} in the Ship of Tools window:\n", left_cut(path, PATH_CAP));
    let tail = "\nThis may or may not be related to the current task.";
    let n = units(&f.text);
    let cut = format!("... (truncated; the range holds {n} characters)");
    let room = CONTEXT_CAP.saturating_sub(units(&lead) + units(tail) + units(&cut));
    let text = if n > TEXT_CUT.min(room) { format!("{}{cut}", head(&f.text, TEXT_CUT.min(room))) } else { f.text.clone() };
    let (pre, post) = ("Selection: ", format!(" lines {a}-{b}"));
    let message = format!("{pre}{}{post}", left_cut(rel, MESSAGE_CAP.saturating_sub(units(pre) + units(&post))));
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": format!("{lead}{text}{tail}"),
        },
        "systemMessage": message,
    })
    .to_string()
}

#[cfg(test)]
#[path = "selection_check_tests.rs"]
mod tests;
