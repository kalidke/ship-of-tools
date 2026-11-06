//! The one normalized chat record (B1 of the project chat): what the daemon says about one Slack message, shared by
//! the daemon and the window. Wire owns this representation; `sot-chat` owns the meaning and builds it from Slack's
//! JSON. A private URL has no field here.

use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAuthor {
    pub id: String,
    pub name: String,
}

/// A file's summary: no URL. A stub (a shared-channel file Slack sent as an id alone) has an empty name and MIME and
/// no size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatFile {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub bytes: Option<u64>,
    pub thumbnail: bool,
}

/// One message. `workspace` and `channel` are part of its identity with `ts`. A field the sender cut to its bound
/// sets `truncated`; `omitted` marks a message whose identity is known but whose content could not be represented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRecord {
    pub workspace: String,
    pub channel: String,
    pub ts: Ts,
    pub thread_ts: Ts,
    pub author: ChatAuthor,
    pub origin: Origin,
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    pub reply_count: u32,
    pub files: Vec<ChatFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_reply: Option<Ts>,
    /// The history page it came from was cut by the plan's horizon (`is_limited`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub limited: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub omitted: bool,
    /// Files past the eighth, not summarized.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub files_omitted: u32,
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
