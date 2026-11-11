//! A chat record as kinded spans: what its poster typed, Slack's markup shown as markup, and nothing in a message able
//! to make a line read as another message's header or badge. Pure; the view that shapes the spans styles each kind.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDateTime};
use sot_protocol::page_reply::unsafe_char;
use sot_protocol::chat::{ChatRecord, Origin, TextSegment, text_segments};

/// A badge's padding left and right of its word: no-break spaces, which the badge's rectangle (the span's run of glyphs)
/// includes, so the padding scales with the text and stays whole where a wrap starts a line with the badge.
const BADGE_PAD: &str = "\u{a0}\u{a0}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Kind {
    /// The header's author name.
    Name,
    /// The header's time.
    Time,
    /// The origin's word, from the record's origin alone.
    Badge,
    Body,
    Markup,
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Span {
    pub kind: Kind,
    pub text: String,
}

/// A record continues the one before it, and draws no header, when both have one author id, name and origin, fall on one
/// local day and are less than `GROUP_GAP` seconds apart (whole seconds). The gap is Slack's grouping rule as remembered: no
/// published source states it (look plan, premise PA), so it is this one constant.
const GROUP_GAP: i64 = 300;

/// A stamp's local time: its whole seconds moved by the offset as integers, so no stamp reaches chrono's zone arithmetic.
/// `None` for a stamp that is not a time.
fn local_stamp(ts: &str, offset: FixedOffset) -> Option<NaiveDateTime> {
    local(ts.split_once('.')?.0.parse().ok()?, offset)
}

fn local(secs: i64, offset: FixedOffset) -> Option<NaiveDateTime> {
    DateTime::from_timestamp(secs.checked_add(i64::from(offset.local_minus_utc()))?, 0).map(|t| t.naive_utc())
}

/// Whether `r` continues `prev`'s group.
pub(super) fn continues(r: &ChatRecord, prev: Option<&ChatRecord>, offset: FixedOffset) -> bool {
    let Some(p) = prev else { return false };
    let (Some(at), Some(before)) = (local_stamp(r.ts.as_str(), offset), local_stamp(p.ts.as_str(), offset)) else { return false };
    r.author == p.author && r.origin == p.origin && at.date() == before.date() && (at - before).num_seconds().abs() < GROUP_GAP
}

/// The header is `name badge time` and ends its line, unless `prev` (the record before it, if one is held) makes this a
/// continuation, which has none; the body follows, then each meta item on its own line. A deleted message (the owner
/// knows it) is its header, if any, and `(message deleted)` alone.
pub(super) fn project(r: &ChatRecord, prev: Option<&ChatRecord>, now: i64, offset: FixedOffset, deleted: bool) -> Vec<Span> {
    let span = |kind, text: String| Span { kind, text };
    let mut out = Vec::new();
    if !continues(r, prev, offset) {
        out.push(span(Kind::Name, format!("{}  ", header_text(&r.author.name))));
        let badge = match r.origin {
            Origin::Drawer => Some("DRAWER"),
            Origin::Row => Some("ROW"),
            Origin::Bot => Some("APP"),
            Origin::Member | Origin::Other => None,
        };
        out.extend(badge.map(|t| span(Kind::Badge, format!("{BADGE_PAD}{t}{BADGE_PAD}"))));
        let gap = if badge.is_some() { "  " } else { "" };
        out.push(span(Kind::Time, format!("{gap}{}\n", when(r.ts.as_str(), now, offset))));
    }
    if deleted {
        out.push(span(Kind::Meta, "(message deleted)".to_string()));
        return out;
    }
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
    // A meta item starts with a line end unless nothing is before it or the text before it already ended one (the header, or
    // a body that did).
    let lead = if out.last().is_none_or(|l| l.text.ends_with('\n')) { "" } else { "\n" };
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

/// `h:mm AM` today, `Nov 9 2:14 PM` earlier (`Nov 9 2022 2:14 PM` in another year), the stamp's own text when its
/// seconds, moved by the offset, are not a time.
pub(super) fn when(ts: &str, now: i64, offset: FixedOffset) -> String {
    match (local_stamp(ts, offset), local(now, offset)) {
        (Some(t), Some(n)) if t.date() == n.date() => t.format("%-I:%M %p").to_string(),
        (Some(t), Some(n)) if t.year() == n.year() => t.format("%b %-d %-I:%M %p").to_string(),
        (Some(t), _) => t.format("%b %-d %Y %-I:%M %p").to_string(),
        _ => ts.to_string(),
    }
}

/// `h:mm` of a stamp under `offset`, for a marked continuation's place in the gutter; `None` for a stamp that is not a time.
pub(super) fn clock(ts: &str, offset: FixedOffset) -> Option<String> {
    Some(local_stamp(ts, offset)?.format("%-I:%M").to_string())
}

fn size(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024.. if (n as f64) < 1023.95 * 1024.0 => format!("{:.1} KiB", n as f64 / 1024.0),
        _ => format!("{:.1} MiB", n as f64 / 1_048_576.0),
    }
}
