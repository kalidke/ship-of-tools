//! Slack message JSON into the wire's [`ChatRecord`]. Every input is hostile: a shape that cannot be represented is
//! an explicit result, never a panic and never a silently dropped message.

use serde_json::{Map, Value};
use sot_protocol::chat::{ChatAuthor, ChatFile, ChatRecord, Origin, Ts};

pub use sot_protocol::chat::{MAX_FILENAME, MAX_FILES, MAX_ID, MAX_MIME, MAX_NAME, MAX_RECORD_BYTES, MAX_TEXT};
pub(crate) use sot_protocol::chat::is_id as valid_id;

/// What the caller knows that the message does not. The own-app identity is the caller's (from `auth.test`): an own
/// post is decided by it alone, never by text or a username a member can type.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub workspace: &'a str,
    pub own_bot_id: Option<&'a str>,
    pub own_user_id: Option<&'a str>,
    /// The history page was cut by the plan's horizon (`is_limited`); every record from it says so.
    pub limited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Normalized {
    /// A message to show and, when new to the caller, to act on.
    New(ChatRecord),
    /// An edit: the message's new content under its original identity. It is never new work.
    Edited(ChatRecord),
    /// A delete: the identity to remove.
    Deleted {
        workspace: String,
        channel: String,
        ts: Ts,
    },
    /// A subtype that is not a message (joins, topic changes, tombstones, ...).
    Unsupported,
    /// No record can carry this: the caller must treat the position as a gap.
    Unrepresentable(Unrepresentable),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unrepresentable {
    NotAnObject,
    Workspace,
    Channel,
    Timestamp,
    /// A field of the wrong type where the shape must be known (`subtype`, the edit's `message`).
    Shape,
}

pub fn normalize(ctx: &Context, channel: &str, raw: &Value) -> Normalized {
    let Some(obj) = raw.as_object() else {
        return Normalized::Unrepresentable(Unrepresentable::NotAnObject);
    };
    if !valid_id(ctx.workspace) {
        return Normalized::Unrepresentable(Unrepresentable::Workspace);
    }
    if !valid_id(channel) {
        return Normalized::Unrepresentable(Unrepresentable::Channel);
    }
    let subtype = match obj.get("subtype") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => return Normalized::Unrepresentable(Unrepresentable::Shape),
    };
    match subtype {
        None | Some("thread_broadcast" | "file_share" | "bot_message" | "me_message") => {
            wrap(build(ctx, channel, obj), Normalized::New)
        }
        Some("message_changed") => {
            let Some(inner) = obj.get("message").and_then(Value::as_object) else {
                return Normalized::Unrepresentable(Unrepresentable::Shape);
            };
            match inner.get("subtype") {
                None | Some(Value::Null) => {}
                Some(Value::String(s))
                    if matches!(
                        s.as_str(),
                        "thread_broadcast" | "file_share" | "bot_message" | "me_message"
                    ) => {}
                Some(Value::String(_)) => return Normalized::Unsupported,
                Some(_) => return Normalized::Unrepresentable(Unrepresentable::Shape),
            }
            wrap(build(ctx, channel, inner), Normalized::Edited)
        }
        Some("message_deleted") => {
            match obj.get("deleted_ts").and_then(Value::as_str).map(Ts::parse) {
                Some(Ok(ts)) => Normalized::Deleted {
                    workspace: ctx.workspace.into(),
                    channel: channel.into(),
                    ts,
                },
                _ => Normalized::Unrepresentable(Unrepresentable::Timestamp),
            }
        }
        Some(_) => Normalized::Unsupported,
    }
}

fn wrap(r: Result<ChatRecord, Unrepresentable>, f: fn(ChatRecord) -> Normalized) -> Normalized {
    r.map_or_else(Normalized::Unrepresentable, f)
}

/// A `Ts` as a number: seconds (at most 20 digits, so they fit) and microseconds. The one order on timestamps is this
/// tuple's: numeric, so `9.000000` precedes `10.000000` and `007.000000` equals `7.000000`.
pub(crate) fn ts_key(ts: &Ts) -> (u128, u32) {
    let (secs, frac) = ts.as_str().split_once('.').unwrap_or(("0", "0"));
    (secs.parse().unwrap_or(0), frac.parse().unwrap_or(0))
}

/// `s` cut to at most `max` bytes on a character boundary, and whether it was cut.
fn cut(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    (&s[..i], true)
}

/// A name-like field: no control characters, trimmed, cut to `max` bytes. The flag is true when it was cut.
pub(crate) fn clean(s: &str, max: usize) -> (String, bool) {
    let plain: String = s.chars().filter(|c| !c.is_control()).collect();
    let (t, cut_) = cut(plain.trim(), max);
    (t.to_string(), cut_)
}

/// A field that is absent or a string; any other type is a shape the record cannot carry.
fn opt_str<'a>(o: &'a Map<String, Value>, k: &str) -> Result<Option<&'a str>, ()> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(()),
    }
}

fn opt_ts(o: &Map<String, Value>, k: &str) -> Result<Option<Ts>, ()> {
    opt_str(o, k)?
        .map(|s| Ts::parse(s).map_err(|_| ()))
        .transpose()
}

fn opt_id<'a>(o: &'a Map<String, Value>, k: &str) -> Result<Option<&'a str>, ()> {
    match opt_str(o, k)? {
        Some(s) if !valid_id(s) => Err(()),
        v => Ok(v),
    }
}

fn build(
    ctx: &Context,
    channel: &str,
    o: &Map<String, Value>,
) -> Result<ChatRecord, Unrepresentable> {
    let ts = opt_ts(o, "ts")
        .ok()
        .flatten()
        .ok_or(Unrepresentable::Timestamp)?;
    let mut rec = match content(ctx, channel, o, &ts) {
        Ok(rec) => rec,
        Err(()) => identity_only(ctx, channel, o, &ts),
    };
    rec.limited = ctx.limited;
    // Escaped control characters can multiply text six-fold, so the text alone is cut until the record fits.
    while !rec.text.is_empty()
        && serde_json::to_string(&rec).map_or(usize::MAX, |s| s.len()) > MAX_RECORD_BYTES
    {
        let (t, _) = cut(&rec.text, rec.text.len() / 2);
        rec.text = t.to_string();
        rec.truncated = true;
    }
    Ok(rec)
}

/// Origin and thread are read field by field, so the one unreadable field does not lose them: an own post stays
/// off `Member` (it is `Bot`, display only), and a reply stays in its thread.
fn lenient_id<'a>(o: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    o.get(k).and_then(Value::as_str).filter(|s| valid_id(s))
}

fn lenient_ts(o: &Map<String, Value>, k: &str) -> Option<Ts> {
    o.get(k)
        .and_then(Value::as_str)
        .and_then(|s| Ts::parse(s).ok())
}

/// Own-app is decided by the supplied bot identity alone; an identity not supplied matches nothing.
fn is_own(ctx: &Context, user: Option<&str>, bot_id: Option<&str>) -> bool {
    (ctx.own_bot_id.is_some() && bot_id == ctx.own_bot_id)
        || (ctx.own_user_id.is_some() && user == ctx.own_user_id)
}

/// A message whose identity is known but whose content could not be read: kept, and marked.
fn identity_only(ctx: &Context, channel: &str, o: &Map<String, Value>, ts: &Ts) -> ChatRecord {
    let user = lenient_id(o, "user");
    let bot_id = lenient_id(o, "bot_id");
    let subtype_bot = o.get("subtype").and_then(Value::as_str) == Some("bot_message");
    let bot_key = o.get("bot_id").is_some_and(|v| !v.is_null());
    let origin = if is_own(ctx, user, bot_id) || bot_key || subtype_bot {
        Origin::Bot
    } else {
        Origin::Member
    };
    ChatRecord {
        workspace: ctx.workspace.into(),
        channel: channel.into(),
        ts: ts.clone(),
        thread_ts: lenient_ts(o, "thread_ts").unwrap_or_else(|| ts.clone()),
        author: ChatAuthor {
            id: user.or(bot_id).unwrap_or("").into(),
            name: String::new(),
        },
        origin,
        text: String::new(),
        truncated: false,
        reply_count: 0,
        files: vec![],
        latest_reply: None,
        limited: false,
        omitted: true,
        files_omitted: 0,
    }
}

fn content(
    ctx: &Context,
    channel: &str,
    o: &Map<String, Value>,
    ts: &Ts,
) -> Result<ChatRecord, ()> {
    let thread_ts = opt_ts(o, "thread_ts")?.unwrap_or_else(|| ts.clone());
    let latest_reply = opt_ts(o, "latest_reply")?;
    let user = opt_id(o, "user")?;
    let bot_id = opt_id(o, "bot_id")?;
    let text = opt_str(o, "text")?.unwrap_or("");
    let reply_count = match o.get("reply_count") {
        None | Some(Value::Null) => 0,
        Some(v) => v
            .as_u64()
            .map_or(Err(()), |n| Ok(u32::try_from(n).unwrap_or(u32::MAX)))?,
    };
    let (files, files_omitted, mut truncated) = files(o.get("files"))?;
    let own = is_own(ctx, user, bot_id);
    let is_bot = bot_id.is_some() || opt_str(o, "subtype")? == Some("bot_message");
    let (origin, name) = if own {
        match own_origin(o)? {
            Some(found) => found,
            None => (Origin::Bot, bot_name(o)?),
        }
    } else if is_bot {
        (Origin::Bot, bot_name(o)?)
    } else {
        (Origin::Member, member_name(o))
    };
    let (name, name_cut) = name;
    let (text, text_cut) = cut(text, MAX_TEXT);
    truncated |= name_cut | text_cut;
    Ok(ChatRecord {
        workspace: ctx.workspace.into(),
        channel: channel.into(),
        ts: ts.clone(),
        thread_ts,
        author: ChatAuthor {
            id: user.or(bot_id).unwrap_or("").into(),
            name,
        },
        origin,
        text: text.into(),
        truncated,
        reply_count,
        files,
        latest_reply,
        limited: false,
        omitted: false,
        files_omitted,
    })
}

/// The origin and the cleaned name (with whether it was cut).
type Recovered = (Origin, (String, bool));

/// Own-app origin and cleaned name, in the order metadata, then the customized username suffix. `Ok(None)`: neither
/// recovers it. Only called for a message the bot identity already marked as the app's own.
fn own_origin(o: &Map<String, Value>) -> Result<Option<Recovered>, ()> {
    if let Some(m) = o.get("metadata").and_then(Value::as_object) {
        if m.get("event_type").and_then(Value::as_str) == Some("sot_chat_post") {
            if let Some(p) = m.get("event_payload").and_then(Value::as_object) {
                let kind = p.get("kind").and_then(Value::as_str).and_then(kind_origin);
                let name = p
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|n| clean(n, MAX_NAME));
                if let (Some(k), Some(n)) = (kind, name) {
                    if !n.0.is_empty() {
                        return Ok(Some((k, n)));
                    }
                }
            }
        }
    }
    if let Some(u) = opt_str(o, "username")? {
        for kind in ["person", "row"] {
            if let Some(name) = u.strip_suffix(&format!(" ({kind} via Ship of Tools)")) {
                let n = clean(name, MAX_NAME);
                if !n.0.is_empty() {
                    return Ok(Some((kind_origin(kind).ok_or(())?, n)));
                }
            }
        }
    }
    Ok(None)
}

fn kind_origin(kind: &str) -> Option<Origin> {
    match kind {
        "person" => Some(Origin::Drawer),
        "row" => Some(Origin::Row),
        _ => None,
    }
}

/// Another bot's name: `username`, then `bot_profile.name`, then a fixed word.
fn bot_name(o: &Map<String, Value>) -> Result<(String, bool), ()> {
    let profile = o
        .get("bot_profile")
        .and_then(Value::as_object)
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str);
    for s in [opt_str(o, "username")?, profile].into_iter().flatten() {
        let n = clean(s, MAX_NAME);
        if !n.0.is_empty() {
            return Ok(n);
        }
    }
    Ok(("bot".to_string(), false))
}

/// A member's name when the event carries a profile; otherwise empty, for the caller to resolve.
fn member_name(o: &Map<String, Value>) -> (String, bool) {
    let profile = o.get("user_profile").and_then(Value::as_object);
    for k in ["display_name", "real_name"] {
        if let Some(s) = profile.and_then(|p| p.get(k)).and_then(Value::as_str) {
            let n = clean(s, MAX_NAME);
            if !n.0.is_empty() {
                return n;
            }
        }
    }
    (String::new(), false)
}

/// Up to eight summaries, how many entries were not summarized, and whether any field was cut.
fn files(v: Option<&Value>) -> Result<(Vec<ChatFile>, u32, bool), ()> {
    let entries = match v {
        None | Some(Value::Null) => return Ok((vec![], 0, false)),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(()),
    };
    let (mut out, mut omitted, mut cut_any) = (Vec::new(), 0u32, false);
    for e in entries {
        let Some(f) = e.as_object() else {
            omitted = omitted.saturating_add(1);
            continue;
        };
        let id = match opt_id(f, "id") {
            Ok(Some(id)) => id,
            _ => {
                omitted = omitted.saturating_add(1);
                continue;
            }
        };
        if out.len() == MAX_FILES {
            omitted = omitted.saturating_add(1);
            continue;
        }
        let stub = f.get("file_access").and_then(Value::as_str) == Some("check_file_info");
        let field =
            |k: &str, max: usize| clean(f.get(k).and_then(Value::as_str).unwrap_or(""), max);
        let (mut name, mut c1) = field("name", MAX_FILENAME);
        if name.is_empty() {
            (name, c1) = field("title", MAX_FILENAME);
        }
        let (mime, c2) = field("mimetype", MAX_MIME);
        let thumbnail = ["thumb_160", "thumb_360", "thumb_480", "thumb_720"]
            .iter()
            .any(|k| f.get(*k).is_some_and(Value::is_string));
        if stub {
            out.push(ChatFile {
                id: id.into(),
                name: String::new(),
                mime: String::new(),
                bytes: None,
                thumbnail: false,
            });
        } else {
            cut_any |= c1 | c2;
            out.push(ChatFile {
                id: id.into(),
                name,
                mime,
                bytes: f.get("size").and_then(Value::as_u64),
                thumbnail,
            });
        }
    }
    Ok((out, omitted, cut_any))
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
