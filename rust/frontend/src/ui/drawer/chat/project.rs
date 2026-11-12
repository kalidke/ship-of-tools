//! A chat record as kinded spans: what its poster typed, Slack's markup shown as markup, and nothing in a message able
//! to make a line read as another message's header or badge. Pure; the view that shapes the spans styles each kind.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime};
use super::mrkdwn::{styled, Role, BLOCK, QUOTE};
use sot_protocol::page_reply::unsafe_char;
use sot_protocol::chat::{ChatRecord, Origin, TextSegment, text_segments};

/// A badge's padding left and right of its word: no-break spaces, which the badge's rectangle (the span's run of glyphs)
/// includes, so the padding scales with the text and stays whole where a wrap starts a line with the badge.
const BADGE_PAD: &str = "\u{a0}\u{a0}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Kind {
    /// A day divider's label, the first line of a record that starts a local day.
    Day,
    /// The header's author name.
    Name,
    /// The header's time.
    Time,
    /// The origin's word, from the record's origin alone.
    Badge,
    Body,
    /// A token Slack's markup has that the drawer does not read, shown as its decoded text.
    Markup,
    /// A link's label or URL, and a mention's or special's name.
    Link,
    Mention,
    Meta,
    /// ` (edited)`, the end of a body Slack marked edited.
    Edited,
    /// A thread summary's reply count, and what follows it on the line.
    Replies,
    Last,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Span {
    pub kind: Kind,
    /// Slack's inline styles and its blocks' and quotes' bits (`mrkdwn`'s), only ever on a body's text and tokens.
    pub style: u8,
    pub text: String,
}

/// A record continues the one before it, and draws no header, when both have one author id, name and origin, fall on one
/// local day and are less than `GROUP_GAP` seconds apart (whole seconds). The gap is Slack's grouping rule as remembered: no
/// published source states it (look plan, premise PA), so it is this one constant.
const GROUP_GAP: i64 = 300;

/// A divider's padding left and right of its label: no-break spaces, glyphs of the label's run, which the pill is drawn around.
const DAY_PAD: &str = "\u{a0}\u{a0}\u{a0}";

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

/// Whether `r` opens a local day: the oldest record shown does, and one whose day is not the record before its.
fn starts_day(r: &ChatRecord, prev: Option<&ChatRecord>, offset: FixedOffset) -> bool {
    let day = |r: &ChatRecord| local_stamp(r.ts.as_str(), offset).map(|t| t.date());
    prev.is_none_or(|p| day(r) != day(p))
}

/// A day's name as Slack says it: `Today`, `Yesterday` or the date. `long` is the divider's form (`Monday, November 9th`,
/// the year after it for another year); the short form runs into a sentence (`today`, `Nov 9`, the year after it for another).
/// `now` is the local date when it is a time.
fn day_word(t: NaiveDate, now: Option<NaiveDate>, long: bool) -> String {
    let year = if now.is_some_and(|n| n.year() == t.year()) { String::new() } else { format!(", {}", t.year()) };
    match (now, long) {
        (Some(n), _) if n == t => if long { "Today" } else { "today" }.to_string(),
        (Some(n), _) if n.pred_opt() == Some(t) => if long { "Yesterday" } else { "yesterday" }.to_string(),
        (_, true) => {
            let d = t.day();
            let ord = match (d % 10, d / 10) { (1, 0 | 2..) => "st", (2, 0 | 2..) => "nd", (3, 0 | 2..) => "rd", _ => "th" };
            format!("{}{ord}{year}", t.format("%A, %B %-d"))
        }
        (_, false) => format!("{}{year}", t.format("%b %-d")),
    }
}

/// The divider's label for a record's stamp, the stamp's own text when it is not a time.
fn day_label(ts: &str, now: i64, offset: FixedOffset) -> String {
    local_stamp(ts, offset).map_or_else(|| ts.to_string(), |t| day_word(t.date(), local(now, offset).map(|n| n.date()), true))
}

/// A thread's last reply as its summary says it: `today at 2:44 PM`, `yesterday at …`, `Nov 9 at …`.
fn last_reply(ts: &str, now: i64, offset: FixedOffset) -> String {
    local_stamp(ts, offset).map_or_else(|| ts.to_string(), |t| format!("{} at {}", day_word(t.date(), local(now, offset).map(|n| n.date()), false), t.format("%-I:%M %p")))
}

/// The header is `name badge time` and ends its line, unless `prev` (the record before it, if one is held) makes this a
/// continuation, which has none, and a divider's line comes first when the record starts a local day; the body follows, then each meta item on its own line. A deleted message (the owner
/// knows it) is its header, if any, and `(message deleted)` alone.
pub(super) fn project(r: &ChatRecord, prev: Option<&ChatRecord>, now: i64, offset: FixedOffset, deleted: bool) -> Vec<Span> {
    let span = |kind, text: String| Span { kind, style: 0, text };
    let mut out = Vec::new();
    if !continues(r, prev, offset) {
        if starts_day(r, prev, offset) {
            out.push(span(Kind::Day, format!("{DAY_PAD}{}{DAY_PAD}\n", day_label(r.ts.as_str(), now, offset))));
        }
        out.push(span(Kind::Name, format!("{}  ", header_text(&r.author.name))));
        let badge = match r.origin {
            Origin::Drawer => Some("DRAWER"),
            Origin::Row => Some("ROW"),
            Origin::Bot => Some("APP"),
            Origin::Member | Origin::Other => None,
        };
        out.extend(badge.map(|t| span(Kind::Badge, format!("{BADGE_PAD}{t}{BADGE_PAD}"))));
        let gap = if badge.is_some() { "  " } else { "" };
        out.push(span(Kind::Time, format!("{gap}{}\n", time_of(r.ts.as_str(), offset))));
    }
    if deleted {
        out.push(span(Kind::Meta, "(message deleted)".to_string()));
        return out;
    }
    let segments = text_segments(&r.text).into_iter().map(|seg| match seg {
        TextSegment::Text(t) => TextSegment::Text(literal(&t)),
        TextSegment::Markup(t) => TextSegment::Markup(literal(&t)),
    });
    let kind = |role| match role { Role::Text => Kind::Body, Role::Link => Kind::Link, Role::Mention => Kind::Mention, Role::Markup => Kind::Markup };
    out.extend(styled(segments).into_iter().map(|p| Span { kind: kind(p.role), style: p.style, text: p.text }));
    // Slack's `(edited)` follows the last word, on its own line after a block or a quote (inside it, it would read as code).
    if r.edited {
        let lead = match out.last() {
            Some(l) if !l.text.ends_with('\n') => if l.style & (BLOCK | QUOTE) != 0 { "\n" } else { " " },
            _ => "",
        };
        out.push(span(Kind::Edited, format!("{lead}(edited)")));
    }
    // Each meta item is one line of spans: its kinds, in order.
    let mut meta: Vec<Vec<(Kind, String)>> = Vec::new();
    for f in &r.files {
        let name = header_text(&f.name);
        meta.push(vec![(Kind::Meta, match (name.is_empty(), f.bytes) {
            (true, _) => "[a file]".to_string(),
            (false, None) => format!("[file {name}]"),
            (false, Some(n)) => format!("[file {name}, {}]", size(n)),
        })]);
    }
    if r.files_omitted > 0 {
        meta.push(vec![(Kind::Meta, format!("[+{} files]", r.files_omitted))]);
    }
    if r.reply_count > 0 {
        let mut summary = vec![(Kind::Replies, match r.reply_count {
            1 => "1 reply".to_string(),
            n => format!("{n} replies"),
        })];
        summary.extend(r.latest_reply.as_ref().map(|t| (Kind::Last, format!("   Last reply {}", last_reply(t.as_str(), now, offset)))));
        meta.push(summary);
    }
    if r.truncated {
        meta.push(vec![(Kind::Meta, "(cut)".to_string())]);
    }
    if r.omitted {
        meta.push(vec![(Kind::Meta, "(this message could not be shown)".to_string())]);
    }
    // A meta item starts with a line end unless nothing is before it or the text before it already ended one (the header, or
    // a body that did).
    let lead = if out.last().is_none_or(|l| l.text.ends_with('\n')) { "" } else { "\n" };
    for (i, item) in meta.into_iter().enumerate() {
        for (j, (kind, text)) in item.into_iter().enumerate() {
            let start = if j > 0 { "" } else if i == 0 { lead } else { "\n" };
            out.push(span(kind, format!("{start}{text}")));
        }
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

/// `h:mm AM` of a stamp under `offset`: the header's time. The stamp's own text when it is not a time.
fn time_of(ts: &str, offset: FixedOffset) -> String {
    local_stamp(ts, offset).map_or_else(|| ts.to_string(), |t| t.format("%-I:%M %p").to_string())
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
