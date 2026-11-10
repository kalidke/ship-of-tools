//! A chat record as kinded spans: what its poster typed, Slack's markup shown as markup, and nothing in a message able
//! to make a line read as another message's header. Pure; the view that shapes the spans styles each kind.

use chrono::{DateTime, FixedOffset};
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
        meta.push(match (f.name.is_empty(), f.bytes) {
            (true, _) => "[a file]".to_string(),
            (false, None) => format!("[file {}]", header_text(&f.name)),
            (false, Some(n)) => format!("[file {}, {}]", header_text(&f.name), size(n)),
        });
    }
    if r.files_omitted > 0 {
        meta.push(format!("[+{} files]", r.files_omitted));
    }
    if r.reply_count > 0 {
        meta.push(format!("{} replies", r.reply_count));
    }
    if r.truncated {
        meta.push("(cut)".to_string());
    }
    if r.omitted {
        meta.push("(this message could not be shown)".to_string());
    }
    out.extend(meta.into_iter().map(|m| span(Kind::Meta, format!("\n{m}"))));
    out
}

/// A name or file name on one line: controls and bidi or other format characters (which can reorder or hide text) go.
fn header_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() && !is_format(*c)).collect()
}

fn is_format(c: char) -> bool {
    matches!(c, '\u{61c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206f}' | '\u{feff}')
}

/// Body text as typed: line ends are LF, tabs stay, any other C0 or C1 control is U+FFFD.
fn literal(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").chars()
        .map(|c| if (c.is_ascii_control() || ('\u{80}'..='\u{9f}').contains(&c)) && c != '\n' && c != '\t' { '\u{fffd}' } else { c })
        .collect()
}

/// `HH:MM` today, `YYYY-MM-DD HH:MM` earlier, the raw stamp when its seconds are not a time.
fn when(ts: &str, now: i64, offset: FixedOffset) -> String {
    let at = |secs: i64| DateTime::from_timestamp(secs, 0).map(|t| t.with_timezone(&offset));
    let secs = ts.split_once('.').and_then(|(s, _)| s.parse::<i64>().ok());
    match (secs.and_then(at), at(now)) {
        (Some(t), Some(n)) if t.date_naive() == n.date_naive() => t.format("%H:%M").to_string(),
        (Some(t), _) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => ts.to_string(),
    }
}

fn size(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} KiB", n as f64 / 1024.0),
        _ => format!("{:.1} MiB", n as f64 / 1_048_576.0),
    }
}
