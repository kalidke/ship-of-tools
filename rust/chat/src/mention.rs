//! A post's `@name`s made Slack's mentions (guarantee 35): the one place an outgoing text gets markup, and the only
//! markup is a token built here from a member id `users.list` gave or a channel-wide word a person typed. Everything
//! typed around it is escaped as before.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sot_protocol::chat::{escape_text, MAX_MENTIONS, MAX_NAME};
use sot_protocol::ops::{ChatUnlinked, ChatUnlinkedWhy};

use crate::api::PostKind;
use crate::normalize::{clean, valid_id};

/// The words that notify a whole channel, each with its token: never a member's name.
const CHANNEL_WIDE: [(&str, &str); 3] = [("channel", "<!channel>"), ("here", "<!here>"), ("everyone", "<!everyone>")];

/// One person of the workspace: their id, the name the drawer shows (`users.info`'s rule), and the names a poster may
/// type: the display name, the real name and the real name's first word.
pub(crate) struct Member {
    id: String,
    shown: String,
    keys: Vec<String>,
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
        let first = real.as_deref().and_then(|r| r.split_whitespace().next()).map(str::to_string);
        Some(Member { id: id.into(), shown, keys: [display, real, first].into_iter().flatten().collect() })
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
            for (k, key) in member.keys.iter().enumerate() {
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
    /// A name reached the member list: `members` mattered.
    pub(crate) wanted: bool,
}

/// `text` escaped, with each `@` a name may follow (the first `MAX_READ`) made a token: a person's `@channel`, `@here` or
/// `@everyone` its channel-wide token (a row's stays text), and any other name the member `members` spells there when
/// every member any of whose names is spelled there, at any length, is that one member. A name that matches no member or
/// several, or a list that could not be read (`Err`), stays text and is named in `unlinked`; so is every link, first,
/// when the linked text would pass `room` bytes, and then the text is sent as typed. Past `MAX_MENTIONS` names, the last
/// entry is `more`.
pub(crate) fn link(text: &str, kind: PostKind, members: Result<&Index, ChatUnlinkedWhy>, room: usize) -> Linked {
    let mut l = Linked { text: String::new(), unlinked: Vec::new(), linked: Vec::new(), wanted: false };
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
                let (len, found) = matched(rest, index);
                match found.as_slice() {
                    [one] => {
                        l.linked.push((one.id.clone(), one.shown.clone()));
                        (len, format!("<@{}>", one.id))
                    }
                    [] => {
                        l.unlinked.push(unlinked(word, ChatUnlinkedWhy::NoMember));
                        continue;
                    }
                    _ => {
                        l.unlinked.push(unlinked(&rest[..len], ChatUnlinkedWhy::SeveralMembers));
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
        let reverted = made.into_iter().map(|name| unlinked(name, ChatUnlinkedWhy::TooLong));
        l.unlinked = reverted.chain(std::mem::take(&mut l.unlinked)).collect();
    }
    let mut seen = HashSet::new();
    l.unlinked.retain(|u| seen.insert((u.name.clone(), u.why)));
    if more || l.unlinked.len() > MAX_MENTIONS {
        l.unlinked.truncate(MAX_MENTIONS - 1);
        l.unlinked.push(unlinked("", ChatUnlinkedWhy::More));
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

/// The members `rest` (the text after an `@`) names: every member any of whose names it spells, at any length, and the
/// longest run spelled, in bytes. Linked only when that is one member, so no name a member chooses (another's name and
/// a comma, or and a following word) takes another member's mention: it makes the mention ambiguous at worst.
fn matched<'m>(rest: &str, index: &'m Index) -> (usize, Vec<&'m Member>) {
    let (mut len, mut found) = (0, Vec::new());
    for &(m, k) in index.by_head.get(&head(rest)).into_iter().flatten() {
        let member = &index.members[m];
        if let Some(l) = spelled(rest, &member.keys[k]) {
            len = len.max(l);
            // A member's names are indexed together, in order, so a repeat is the last one found.
            if found.last().is_none_or(|f: &&Member| !std::ptr::eq(*f, member)) {
                found.push(member);
            }
        }
    }
    (len, found)
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
/// not a letter, a digit, a combining mark or `-`, or a `.` or `_` that ends a word (`@Ada.` and `_@Ada_` are Ada,
/// `@ada.example` and `@ada_x` are no one).
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
        Some('.' | '_') => !after.next().is_some_and(word),
        Some(c) => !(word(c) || c == '-'),
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
    ChatUnlinked { name: clean(name, MAX_NAME).0, why }
}

#[cfg(test)]
#[path = "mention_tests.rs"]
mod tests;
