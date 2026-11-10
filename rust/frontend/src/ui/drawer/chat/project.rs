//! A chat record as kinded spans: what its poster typed, Slack's markup shown as markup, and nothing in a message able
//! to make a line read as another message's header. Pure; the view that shapes the spans styles each kind.

use chrono::{DateTime, FixedOffset};
use sot_protocol::page_reply::unsafe_char;
use sot_protocol::chat::{ChatRecord, Origin, TextSegment, text_segments};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// The author name and time line; the flag is B5o's mark.
    Header(bool),
    Origin,
    Body,
    Markup,
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Span {
    pub kind: Kind,
    pub text: String,
}

/// The header is `name [origin] time` and ends its line; the body follows, then each meta item on its own line.
pub(super) fn project(r: &ChatRecord, now: i64, offset: FixedOffset, marked: bool) -> Vec<Span> {
    let span = |kind, text: String| Span { kind, text };
    let mut out = vec![span(Kind::Header(marked), format!("{} ", header_text(&r.author.name)))];
    let tag = match r.origin {
        Origin::Drawer => Some("[drawer] "),
        Origin::Row => Some("[row] "),
        Origin::Bot => Some("[bot] "),
        Origin::Member | Origin::Other => None,
    };
    out.extend(tag.map(|t| span(Kind::Origin, t.to_string())));
    out.push(span(Kind::Header(marked), format!("{}\n", when(r.ts.as_str(), now, offset))));
    for seg in text_segments(&r.text) {
        out.push(match seg {
            TextSegment::Text(t) => span(Kind::Body, literal(&t)),
            TextSegment::Markup(t) => span(Kind::Markup, literal(&t)),
        });
    }
    let mut meta = Vec::new();
    for f in &r.files {
        let name = header_text(&f.name);
        meta.push(match (name.is_empty(), f.bytes) {
            (true, _) => "[a file]".to_string(),
            (false, None) => format!("[file {name}]"),
            (false, Some(n)) => format!("[file {name}, {}]", size(n)),
        });
    }
    if r.files_omitted > 0 {
        meta.push(format!("[+{} files]", r.files_omitted));
    }
    if r.reply_count > 0 {
        meta.push(match r.reply_count {
            1 => "1 reply".to_string(),
            n => format!("{n} replies"),
        });
    }
    if r.truncated {
        meta.push("(cut)".to_string());
    }
    if r.omitted {
        meta.push("(this message could not be shown)".to_string());
    }
    // A meta item starts with a line end unless the text before it already ended one (the header, or a body that did).
    let lead = if out.last().is_some_and(|l| l.text.ends_with('\n')) { "" } else { "\n" };
    for (i, m) in meta.into_iter().enumerate() {
        out.push(span(Kind::Meta, format!("{}{m}", if i == 0 { lead } else { "\n" })));
    }
    out
}

/// A name or file name on one line: the wire's one predicate removes controls, line separators and direction or other
/// invisible formatting characters.
fn header_text(s: &str) -> String {
    s.chars().filter(|c| !unsafe_char(*c)).collect()
}

/// Body text as typed: line ends are LF, tabs stay, any other C0 or C1 control is U+FFFD.
fn literal(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").chars()
        .map(|c| if c.is_control() && c != '\n' && c != '\t' { '\u{fffd}' } else { c })
        .collect()
}

/// `HH:MM` today, `YYYY-MM-DD HH:MM` earlier, the stamp's own text when its seconds, moved by the offset, are not a
/// time. The offset is added as integers, so no stamp reaches chrono's zone arithmetic.
fn when(ts: &str, now: i64, offset: FixedOffset) -> String {
    let local = |secs: i64| {
        secs.checked_add(i64::from(offset.local_minus_utc())).and_then(|l| DateTime::from_timestamp(l, 0)).map(|t| t.naive_utc())
    };
    let at = ts.split_once('.').and_then(|(s, _)| s.parse::<i64>().ok()).and_then(local);
    match (at, local(now)) {
        (Some(t), Some(n)) if t.date() == n.date() => t.format("%H:%M").to_string(),
        (Some(t), _) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => ts.to_string(),
    }
}

fn size(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024.. if (n as f64) < 1023.95 * 1024.0 => format!("{:.1} KiB", n as f64 / 1024.0),
        _ => format!("{:.1} MiB", n as f64 / 1_048_576.0),
    }
}
