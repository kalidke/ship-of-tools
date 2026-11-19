//! A post's `@name`s made Slack's mentions (guarantee 35): the one place an outgoing text gets markup, and the only
//! markup is a token built here from a member id `users.list` gave or a channel-wide word a person typed. Everything
//! typed around it is escaped as before.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sot_protocol::chat::{escape_text, MAX_MENTIONS, MAX_NAME};
use sot_protocol::ops::{ChatUnlinked, ChatUnlinkedWhy, MAX_CANDIDATES};

use crate::api::PostKind;
use crate::normalize::{clean, valid_id};

/// The words that notify a whole channel, each with its token: never a member's name.
const CHANNEL_WIDE: [(&str, &str); 3] = [("channel", "<!channel>"), ("here", "<!here>"), ("everyone", "<!everyone>")];

/// One person of the workspace: their id, the name the drawer shows (`users.info`'s rule), and the names a poster may
/// type, each marked whole or not: the display name and the real name (whole), and the real name's first word when the
/// real name has more than one.
pub(crate) struct Member {
    id: String,
    shown: String,
    /// What the reply's `mentioned` says of them: `display (real)` when both exist and differ ignoring case, else `shown`.
    told: String,
    keys: Vec<(String, bool)>,
}

impl Member {
    /// A `users.list` entry that is a person: a valid id, not a bot, not deactivated, not Slackbot, with a name.
    pub(crate) fn of(raw: &Value) -> Option<Member> {
        let id = raw.get("id").and_then(Value::as_str).filter(|id| valid_id(id) && *id != "USLACKBOT")?;
        if [raw.get("deleted"), raw.get("is_bot")].contains(&Some(&Value::Bool(true))) {
            return None;
        }
        let field = |v: Option<&Value>, k: &str| {
            v.and_then(|h| h.get(k)).and_then(Value::as_str).map(|s| clean(s, MAX_NAME).0).filter(|s| !s.is_empty())
        };
        let (display, real) = (field(raw.get("profile"), "display_name"), field(raw.get("profile"), "real_name"));
        let shown = display.clone().or_else(|| real.clone()).or_else(|| field(Some(raw), "name"))?;
        let told = match (&display, &real) {
            (Some(d), Some(r)) if d.to_lowercase() != r.to_lowercase() => clean(&format!("{d} ({r})"), MAX_NAME).0,
            _ => shown.clone(),
        };
        let first = real.as_deref().and_then(|r| r.split_once(char::is_whitespace)).map(|(f, _)| (f.to_string(), false));
        let whole = [display, real].into_iter().flatten().map(|k| (k, true));
        Some(Member { id: id.into(), shown, told, keys: whole.chain(first).collect() })
    }
}

/// The workspace's people as one `users.list` walk read them, indexed, or the fact that there were more than it reads.
pub(crate) enum Roster {
    Read(Index),
    TooMany,
}

/// The members and, by the lowercased leading word (`head`) of each of their names, which member and which name: built
/// once per walk, so a candidate reads only the names that begin as it does.
pub(crate) struct Index {
    members: Vec<Member>,
    by_head: HashMap<String, Vec<(usize, usize)>>,
}

impl Index {
    pub(crate) fn new(members: Vec<Member>) -> Index {
        let mut by_head: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        for (m, member) in members.iter().enumerate() {
            for (k, (key, _)) in member.keys.iter().enumerate() {
                by_head.entry(head(key)).or_default().push((m, k));
            }
        }
        Index { members, by_head }
    }
}

/// The most `@` names one post reads; the rest stay text, named by one `more` entry.
pub(crate) const MAX_READ: usize = 50;

/// A post's text as Slack's: `text` fits the room it was given, unless it was all typed text (the caller's bound).
pub(crate) struct Linked {
    pub(crate) text: String,
    pub(crate) unlinked: Vec<ChatUnlinked>,
    /// Each member linked, as `(id, shown name)`, for the name cache.
    pub(crate) linked: Vec<(String, String)>,
    /// The members linked, in the text's order, each as its `told` name, at most `MAX_MENTIONS` (an empty last name: more).
    pub(crate) mentioned: Vec<String>,
    /// A name reached the member list: `members` mattered.
    pub(crate) wanted: bool,
}

/// `text` escaped, with each `@` a name may follow (the first `MAX_READ`) made a token: a person's `@channel`, `@here` or
/// `@everyone` its channel-wide token (a row's stays text), and any other name the one member `matched` finds there. A
/// name that matches no member or several, or a list that could not be read (`Err`), stays text and is named in
/// `unlinked`; so is every link, first, when the linked text would pass `room` bytes, and then the text is sent as
/// typed. Past `MAX_MENTIONS` names, the last entry is `more`.
pub(crate) fn link(text: &str, kind: PostKind, members: Result<&Index, ChatUnlinkedWhy>, room: usize) -> Linked {
    let mut l = Linked { text: String::new(), unlinked: Vec::new(), linked: Vec::new(), mentioned: Vec::new(), wanted: false };
    let (mut done, mut made, mut read, mut more) = (0, Vec::new(), 0, false);
    for at in candidates(text) {
        let rest = &text[at + 1..];
        let word = typed(rest);
        if at < done || word.is_empty() {
            continue;
        }
        if read == MAX_READ {
            more = true;
            break;
        }
        read += 1;
        let wide = CHANNEL_WIDE.iter().find_map(|(w, token)| spelled(rest, w).map(|len| (len, *token)));
        let (len, token) = match (wide, members) {
            (Some(wide), _) if kind == PostKind::Person => (wide.0, wide.1.to_string()),
            (Some(_), _) => {
                l.unlinked.push(unlinked(word, ChatUnlinkedWhy::ChannelWide));
                continue;
            }
            (None, Err(why)) => {
                l.wanted = true;
                l.unlinked.push(unlinked(word, why));
                continue;
            }
            (None, Ok(index)) => {
                l.wanted = true;
                match matched(rest, index) {
                    Found::One(one, len) => {
                        l.linked.push((one.id.clone(), one.shown.clone()));
                        l.mentioned.push(one.told.clone());
                        (len, format!("<@{}>", one.id))
                    }
                    Found::Nobody => {
                        l.unlinked.push(unlinked(word, ChatUnlinkedWhy::NoMember));
                        continue;
                    }
                    Found::Several(len, all) => {
                        let mut entry = unlinked(&rest[..len], ChatUnlinkedWhy::SeveralMembers);
                        entry.candidates = all.iter().take(MAX_CANDIDATES).map(|m| m.shown.clone()).collect();
                        entry.candidates.extend((all.len() > MAX_CANDIDATES).then(String::new));
                        l.unlinked.push(entry);
                        continue;
                    }
                }
            }
        };
        made.push(&rest[..len]);
        l.text.push_str(&escape_text(&text[done..at]));
        l.text.push_str(&token);
        done = at + 1 + len;
    }
    l.text.push_str(&escape_text(&text[done..]));
    if l.text.len() > room {
        l.text = escape_text(text);
        l.linked.clear();
        l.mentioned.clear();
        let reverted = made.into_iter().map(|name| unlinked(name, ChatUnlinkedWhy::TooLong));
        l.unlinked = reverted.chain(std::mem::take(&mut l.unlinked)).collect();
    }
    let mut seen = HashSet::new();
    l.unlinked.retain(|u| seen.insert((u.name.clone(), u.why)));
    if more || l.unlinked.len() > MAX_MENTIONS {
        l.unlinked.truncate(MAX_MENTIONS - 1);
        l.unlinked.push(unlinked("", ChatUnlinkedWhy::More));
    }
    if l.mentioned.len() > MAX_MENTIONS {
        l.mentioned.truncate(MAX_MENTIONS);
        l.mentioned[MAX_MENTIONS - 1] = String::new();
    }
    l
}

/// The byte offset of each `@` a name may follow: at the start, or after a space or an opening mark; never inside code
/// (a span between two backquotes on one line, a block between two fences), on a quoted line (`>` first) or after `>>>`.
fn candidates(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let (mut i, mut quoted) = (0, false);
    while i < text.len() {
        let rest = &text[i..];
        if i == 0 || text[..i].ends_with('\n') {
            if rest.starts_with(">>>") {
                break;
            }
            quoted = rest.starts_with('>');
        }
        if rest.starts_with('`') {
            let line = rest.find('\n').unwrap_or(rest.len());
            let code = match rest.strip_prefix("```") {
                Some(after) => after.find("```").map(|end| 6 + end),
                None => rest[1..line].find('`').map(|end| 2 + end),
            };
            if let Some(skip) = code {
                i += skip;
                continue;
            }
        }
        let opens = text[..i].chars().next_back().is_none_or(|c| c.is_whitespace() || "([{\"'*_~".contains(c));
        if rest.starts_with('@') && !quoted && opens {
            out.push(i);
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// What an `@` names.
enum Found<'m> {
    One(&'m Member, usize),
    /// The longest run spelled there, in bytes, and the members it could be.
    Several(usize, Vec<&'m Member>),
    Nobody,
}

/// The member `rest` (the text after an `@`) names. W is the members one of whose whole names (display or real) is
/// spelled there, at any length, and F those spelled there only by their first word. If W is not empty only W counts:
/// the longest run any of W is spelled by links when exactly one member of W is spelled at that length and every other
/// member of W is spelled only by runs the longest one continues with a space (`Ben` beside `Ben Sample`); anything else
/// in W is several. Else one member of F links, and more are several.
fn matched<'m>(rest: &str, index: &'m Index) -> Found<'m> {
    // Per member, in the index's order (a member's names are indexed together): its whole names' lengths, its first word's.
    let mut spelled_here: Vec<(&Member, Vec<usize>, Option<usize>)> = Vec::new();
    for &(m, k) in index.by_head.get(&head(rest)).into_iter().flatten() {
        let member = &index.members[m];
        let (key, whole) = &member.keys[k];
        let Some(len) = spelled(rest, key) else { continue };
        if !spelled_here.last().is_some_and(|(last, _, _)| std::ptr::eq(*last, member)) {
            spelled_here.push((member, Vec::new(), None));
        }
        let (_, wholes, first) = spelled_here.last_mut().expect("pushed above");
        if *whole { wholes.push(len) } else { *first = Some(len) }
    }
    let (w, f): (Vec<_>, Vec<_>) = spelled_here.into_iter().partition(|(_, wholes, _)| !wholes.is_empty());
    if let Some(longest) = w.iter().flat_map(|(_, wholes, _)| wholes.iter().copied()).max() {
        let at_longest: Vec<&Member> = w.iter().filter(|(_, wholes, _)| wholes.contains(&longest)).map(|(m, _, _)| *m).collect();
        // A shorter name the longest one extends after a space (by further words), not by punctuation.
        let extended = |len: usize| rest[len..longest].starts_with(char::is_whitespace);
        let others_extended = w.iter().filter(|(m, _, _)| !std::ptr::eq(*m, at_longest[0])).all(|(_, wholes, _)| wholes.iter().all(|&l| extended(l)));
        return match at_longest.as_slice() {
            [one] if others_extended => Found::One(one, longest),
            _ => Found::Several(longest, w.into_iter().map(|(m, _, _)| m).collect()),
        };
    }
    let longest = f.iter().filter_map(|(_, _, first)| *first).max().unwrap_or(0);
    match f.as_slice() {
        [] => Found::Nobody,
        [(one, _, Some(len))] => Found::One(one, *len),
        _ => Found::Several(longest, f.into_iter().map(|(m, _, _)| m).collect()),
    }
}

/// The lowercased letters and digits a name or a text begins with: the index's key.
fn head(s: &str) -> String {
    s.chars().take_while(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A combining mark (the common blocks): part of the word it follows, so `Ada` and a decomposed accent is not `Ada`.
fn is_mark(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{1DC0}'..='\u{1DFF}' | '\u{20D0}'..='\u{20FF}' | '\u{FE20}'..='\u{FE2F}')
}

/// The bytes of `rest` that spell `name`, ignoring case, when a boundary follows them: the end, or a character that is
/// not a letter, a digit or a combining mark, or a `-`, `.` or `_` that ends a word (`@Ada.`, `@Ada-` and `_@Ada_` are
/// Ada, `@ada.example`, `@Ada-Lee` and `@ada_x` are no one).
fn spelled(rest: &str, name: &str) -> Option<usize> {
    let mut chars = rest.char_indices();
    let mut end = 0;
    for want in name.chars() {
        let (i, c) = chars.next()?;
        if !c.to_lowercase().eq(want.to_lowercase()) {
            return None;
        }
        end = i + c.len_utf8();
    }
    let word = |c: char| c.is_alphanumeric() || is_mark(c);
    let mut after = rest[end..].chars();
    let bounded = match after.next() {
        None => true,
        Some('-' | '.' | '_') => !after.next().is_some_and(word),
        Some(c) => !word(c),
    };
    bounded.then_some(end)
}

/// The name typed after an `@`, as the poster is told it: letters, digits, marks, `_`, `-` and inner dots.
fn typed(rest: &str) -> &str {
    let end = rest.find(|c: char| !(c.is_alphanumeric() || is_mark(c) || matches!(c, '_' | '-' | '.'))).unwrap_or(rest.len());
    rest[..end].trim_end_matches(['.', '_'])
}

/// A name left as text, within `MAX_NAME`.
fn unlinked(name: &str, why: ChatUnlinkedWhy) -> ChatUnlinked {
    ChatUnlinked { name: clean(name, MAX_NAME).0, why, candidates: Vec::new() }
}

#[cfg(test)]
#[path = "mention_tests.rs"]
mod tests;
