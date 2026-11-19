//! A post's `@name`s made Slack's mentions (guarantee 35): the one place an outgoing text gets markup, and the only
//! markup is a token built here from a member id `users.list` gave or a channel-wide word a person typed. Everything
//! typed around it is escaped as before.

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

/// The workspace's people as one `users.list` walk read them, or the fact that there were more than it reads.
pub(crate) enum Roster {
    Read(Vec<Member>),
    TooMany,
}

/// A post's text as Slack's: `text` fits the room it was given, unless it was all typed text (the caller's bound).
pub(crate) struct Linked {
    pub(crate) text: String,
    pub(crate) unlinked: Vec<ChatUnlinked>,
    /// Each member linked, as `(id, shown name)`, for the name cache.
    pub(crate) linked: Vec<(String, String)>,
    /// A name reached the member list: `members` mattered.
    pub(crate) wanted: bool,
}

/// `text` escaped, with each `@` a name may follow made a token: a person's `@channel`, `@here` or `@everyone` its
/// channel-wide token (a row's stays text), and any other name the one member `members` spells there. A name that
/// matches no member or several, or a list that could not be read (`Err`), stays text and is named in `unlinked`; so is
/// every link when the linked text would pass `room` bytes, and then the text is sent as typed.
pub(crate) fn link(text: &str, kind: PostKind, members: Result<&[Member], ChatUnlinkedWhy>, room: usize) -> Linked {
    let mut l = Linked { text: String::new(), unlinked: Vec::new(), linked: Vec::new(), wanted: false };
    let (mut done, mut made) = (0, Vec::new());
    for at in candidates(text) {
        let rest = &text[at + 1..];
        let word = typed(rest);
        if at < done || word.is_empty() {
            continue;
        }
        let wide = CHANNEL_WIDE.iter().find_map(|(w, token)| spelled(rest, w).map(|len| (len, *token)));
        let (len, token) = match (wide, members) {
            (Some(wide), _) if kind == PostKind::Person => (wide.0, wide.1.to_string()),
            (Some(_), _) => {
                note(&mut l.unlinked, word, ChatUnlinkedWhy::ChannelWide);
                continue;
            }
            (None, Err(why)) => {
                l.wanted = true;
                note(&mut l.unlinked, word, why);
                continue;
            }
            (None, Ok(members)) => {
                l.wanted = true;
                let (len, found) = matched(rest, members);
                match found.as_slice() {
                    [one] => {
                        l.linked.push((one.id.clone(), one.shown.clone()));
                        (len, format!("<@{}>", one.id))
                    }
                    [] => {
                        note(&mut l.unlinked, word, ChatUnlinkedWhy::NoMember);
                        continue;
                    }
                    _ => {
                        note(&mut l.unlinked, &rest[..len], ChatUnlinkedWhy::SeveralMembers);
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
        made.into_iter().for_each(|name| note(&mut l.unlinked, name, ChatUnlinkedWhy::TooLong));
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

/// The member `rest` (the text after an `@`) names: those whose longest name it spells, as a byte length.
fn matched<'m>(rest: &str, members: &'m [Member]) -> (usize, Vec<&'m Member>) {
    let mut best: (usize, Vec<&Member>) = (0, Vec::new());
    for m in members {
        let Some(len) = m.keys.iter().filter_map(|k| spelled(rest, k)).max() else { continue };
        if len > best.0 {
            best = (len, vec![m]);
        } else if len == best.0 && !best.1.iter().any(|b| b.id == m.id) {
            best.1.push(m);
        }
    }
    best
}

/// The bytes of `rest` that spell `name`, ignoring case, when a boundary follows them: the end, or a character that is
/// not a letter, a digit, `_` or `-`, or a dot that ends a word (`@Ada.` is Ada, `@ada.example` is no one).
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
    let mut after = rest[end..].chars();
    let bounded = match after.next() {
        None => true,
        Some('.') => !after.next().is_some_and(char::is_alphanumeric),
        Some(c) => !(c.is_alphanumeric() || c == '_' || c == '-'),
    };
    bounded.then_some(end)
}

/// The name typed after an `@`, as the poster is told it: letters, digits, `_`, `-` and inner dots.
fn typed(rest: &str) -> &str {
    let end = rest.find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))).unwrap_or(rest.len());
    rest[..end].trim_end_matches('.')
}

/// Adds a name left as text, once, and at most `MAX_MENTIONS` of them.
fn note(unlinked: &mut Vec<ChatUnlinked>, name: &str, why: ChatUnlinkedWhy) {
    let name = clean(name, MAX_NAME).0;
    if unlinked.len() < MAX_MENTIONS && !unlinked.iter().any(|u| u.name == name && u.why == why) {
        unlinked.push(ChatUnlinked { name, why });
    }
}

#[cfg(test)]
#[path = "mention_tests.rs"]
mod tests;
