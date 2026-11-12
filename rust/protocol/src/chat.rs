//! The one normalized chat record (B1 of the project chat): what the daemon says about one Slack message, shared by
//! the daemon and the window. Wire owns this representation and the grammar of its text; `sot-chat` builds it from
//! Slack's JSON. A private URL has no field here.

use serde::de::{Deserializer, Error, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

/// The record's bounds, the one copy: a decoder enforces them (and holds nothing past them) and `sot-chat` builds a
/// record within them, by using these constants. A record's encoded size is also held to `MAX_RECORD_BYTES` by
/// `sot-chat`, not by the decoder: the per-field bounds alone allow about 72 KiB.
pub const MAX_RECORD_BYTES: usize = 32 * 1024;
pub const MAX_ID: usize = 64;
pub const MAX_TEXT: usize = 8 * 1024;
pub const MAX_FILES: usize = 8;
pub const MAX_NAME: usize = 160;
pub const MAX_MENTIONS: usize = 16;
pub const MAX_FILENAME: usize = 256;
pub const MAX_MIME: usize = 128;

/// An id as Slack and this app spell one: 1 to `MAX_ID` ASCII letters, digits, `_` and `-`.
pub fn is_id(s: &str) -> bool {
    (1..=MAX_ID).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `deserialize_with` for a string of at most `N` bytes.
pub(crate) fn within<'de, D: Deserializer<'de>, const N: usize>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    if s.len() > N {
        return Err(D::Error::custom(format_args!("more than {N} bytes")));
    }
    Ok(s)
}

/// `deserialize_with` for a list of at most `N` items; the item after the `N`th is decoded, then refused.
pub(crate) fn at_most<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(d: D) -> Result<Vec<T>, D::Error> {
    struct Cap<T, const N: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for Cap<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "a list of at most {N}")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            let mut out = Vec::new();
            while let Some(item) = seq.next_element()? {
                if out.len() == N {
                    return Err(A::Error::custom(format_args!("more than {N} items")));
                }
                out.push(item);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Cap::<T, N>(std::marker::PhantomData))
}

/// A Slack timestamp: exactly `<seconds>.<six digits>`, kept as the decimal string, never a float. Slack answers a
/// bound with any other number of fraction digits as an empty page and no error, so a bad bound must be refused
/// before it is sent; this type is the only way to hold one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Ts(String);

/// Why a string is not a [`Ts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadTs;

impl Ts {
    /// 1 to 20 digits of seconds, a dot, six digits: at most 27 bytes, inside the 32-byte bound.
    pub fn parse(s: &str) -> Result<Ts, BadTs> {
        let (secs, frac) = s.split_once('.').ok_or(BadTs)?;
        let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
        if (1..=20).contains(&secs.len()) && digits(secs) && frac.len() == 6 && digits(frac) {
            Ok(Ts(s.to_string()))
        } else {
            Err(BadTs)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Ts {
    type Error = String;
    fn try_from(s: String) -> Result<Ts, String> {
        Ts::parse(&s).map_err(|_| "a timestamp is <seconds>.<six digits>".to_string())
    }
}

impl From<Ts> for String {
    fn from(t: Ts) -> String {
        t.0
    }
}

/// Who wrote a message, as the daemon classifies it. `Drawer` and `Row` are this app's own posts made for a person
/// at the window and for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Member,
    Drawer,
    Row,
    Bot,
    /// An origin added later: a reader shows it as another's message, never as this app's own post.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAuthor {
    #[serde(deserialize_with = "within::<_, MAX_ID>")]
    pub id: String,
    #[serde(deserialize_with = "within::<_, MAX_NAME>")]
    pub name: String,
}

/// A person a message's text mentions, by the name the daemon looked up: `id` is the `<@U…>` token's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatName {
    #[serde(deserialize_with = "an_id")]
    pub id: String,
    #[serde(deserialize_with = "within::<_, MAX_NAME>")]
    pub name: String,
}

/// `deserialize_with` for an id: `is_id`, which the length bound is part of.
fn an_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    if !is_id(&s) {
        return Err(D::Error::custom("not an id"));
    }
    Ok(s)
}

/// A file's summary: no URL. A stub (a shared-channel file Slack sent as an id alone) has an empty name and MIME and
/// no size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatFile {
    #[serde(deserialize_with = "within::<_, MAX_ID>")]
    pub id: String,
    #[serde(deserialize_with = "within::<_, MAX_FILENAME>")]
    pub name: String,
    #[serde(deserialize_with = "within::<_, MAX_MIME>")]
    pub mime: String,
    pub bytes: Option<u64>,
    pub thumbnail: bool,
}

/// One message. `workspace` and `channel` are part of its identity with `ts`. A field the sender cut to its bound
/// sets `truncated`; `omitted` marks a message whose identity is known but whose content could not be represented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRecord {
    #[serde(deserialize_with = "within::<_, MAX_ID>")]
    pub workspace: String,
    #[serde(deserialize_with = "within::<_, MAX_ID>")]
    pub channel: String,
    pub ts: Ts,
    pub thread_ts: Ts,
    pub author: ChatAuthor,
    pub origin: Origin,
    #[serde(deserialize_with = "within::<_, MAX_TEXT>")]
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    pub reply_count: u32,
    #[serde(deserialize_with = "at_most::<_, _, MAX_FILES>")]
    pub files: Vec<ChatFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_reply: Option<Ts>,
    /// The history page it came from was cut by the plan's horizon (`is_limited`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub limited: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub omitted: bool,
    /// File entries not summarized: those past the eighth, and any entry that is not an object or has no valid id.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub files_omitted: u32,
    /// Slack marked the message edited (its `edited` object); the record carries the fact, not who or when.
    #[serde(default, skip_serializing_if = "is_false")]
    pub edited: bool,
    /// The people the text mentions whose names are known, at most `MAX_MENTIONS`, one per id; a mention not here shows
    /// its label or its id.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "at_most::<_, _, MAX_MENTIONS>")]
    pub names: Vec<ChatName>,
}

#[cfg(any(test, feature = "test-support"))]
impl ChatRecord {
    /// A member's one-line top-level message, every optional field empty: a test writes
    /// `ChatRecord { text: ..., ..ChatRecord::fixture() }`, so a field added later is added here once.
    pub fn fixture() -> ChatRecord {
        let ts = Ts::parse("1700000000.000100").unwrap();
        ChatRecord {
            workspace: "T1".into(),
            channel: "C1".into(),
            thread_ts: ts.clone(),
            ts,
            author: ChatAuthor { id: "U1".into(), name: "Ada".into() },
            origin: Origin::Member,
            text: "hi".into(),
            truncated: false,
            reply_count: 0,
            files: Vec::new(),
            latest_reply: None,
            limited: false,
            omitted: false,
            files_omitted: 0,
            edited: false,
            names: Vec::new(),
        }
    }
}

/// One piece of a record's text: a token Slack delimited with `<` and `>`, or the words around it. Both are decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextSegment {
    Markup(String),
    Text(String),
}

/// Splits Slack's text, which is escaped markup. A bare `<` always opens a token, closed by the next `>` or by the end
/// of the text (an unclosed one is a token a byte cut split). Only then are `&lt;`, `&gt;` and `&amp;` decoded, in one
/// pass, in each segment; every other `&...` stays. No segment is empty.
pub fn text_segments(text: &str) -> Vec<TextSegment> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let markup = rest.starts_with('<');
        let end = if markup {
            rest.find('>').map_or(rest.len(), |i| i + 1)
        } else {
            rest.find('<').unwrap_or(rest.len())
        };
        let decoded = decode_entities(&rest[..end]);
        out.push(if markup { TextSegment::Markup(decoded) } else { TextSegment::Text(decoded) });
        rest = &rest[end..];
    }
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// A person's plain text as Slack's: `&`, `<` and `>` become `&amp;`, `&lt;` and `&gt;`, in one pass.
pub fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The one bound on a post: its escaped form fits `MAX_TEXT`. The draft and chat's door both ask it.
pub fn post_text_fits(text: &str) -> bool {
    escape_text(text).len() <= MAX_TEXT
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
