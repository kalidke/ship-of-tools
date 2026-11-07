// the project chat's operations and events (B2d): what the window and the daemon say about one watched channel

use super::*;
use crate::chat::{at_most, is_id, within, ChatRecord, Ts, MAX_ID, MAX_NAME, MAX_TEXT};

/// The most messages a history page asks for or holds.
pub const MAX_PAGE: u16 = 20;
/// The most records one page, or one `chat.changed` event, carries: a page and a thread's parent.
pub const MAX_PAGE_RECORDS: usize = MAX_PAGE as usize + 1;
/// A history cursor; the daemon binds it to its whole query.
pub const MAX_CURSOR: usize = 1024;
/// The most channels watched at once (the watch model's bound), so the most a status can name.
pub const MAX_WATCHED: usize = 32;
/// The longest wait a `limited` post reply can name, in seconds.
pub const MAX_RETRY_SECS: u64 = 3600;
const MAX_ROW: usize = 256;

/// A channel id: 1 to 64 ASCII letters, digits, `_` and `-`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChatChannel(String);

impl TryFrom<String> for ChatChannel {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if is_id(&s) { Ok(ChatChannel(s)) } else { Err("not a channel id".to_string()) }
    }
}

impl From<ChatChannel> for String {
    fn from(c: ChatChannel) -> String {
        c.0
    }
}

impl ChatChannel {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a request acts on: the canonical row and the (Slack workspace, channel) pair the caller expects it to map to.
/// The daemon rechecks both before acting; a mismatch is `stale_row` or `stale_mapping`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTarget {
    #[serde(deserialize_with = "within::<_, MAX_ROW>")]
    pub row: String,
    #[serde(deserialize_with = "within::<_, MAX_ID>")]
    pub workspace: String,
    pub channel: ChatChannel,
}

/// The fixed refusal vocabulary, never Slack's or a library's text. A refused request is the standard error payload
/// `{error, code}` whose `code` is one of these; `other` is what a reader takes a code added later for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatCode {
    /// No chat service runs here (not the hub, not configured, stopped).
    Unavailable,
    /// The row's folder maps to no channel (the inert anchor included).
    Unmapped,
    StaleRow,
    StaleMapping,
    BadRequest,
    TooManyWatches,
    Limited,
    Busy,
    Failed,
    #[serde(other)]
    Other,
}

/// The body of a refusal, as a reader decodes it (`error`, the fixed text, is ignored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRefusal {
    /// Absent in an older daemon's unknown-op refusal (`{error}` alone), the answer to a chat op it does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ChatCode>,
}

/// `chat.resolve`: what is mapped to this row. The only request that names no expected pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResolveReq {
    #[serde(deserialize_with = "within::<_, MAX_ROW>")]
    pub row: String,
}

/// Reply to `chat.resolve`. `delivers`: mail may be delivered for the row (a `mail_after` is set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChatResolveRes {
    Mapped {
        #[serde(deserialize_with = "within::<_, MAX_ID>")]
        workspace: String,
        channel: ChatChannel,
        delivers: bool,
    },
    Unmapped,
    /// Also a state added later: a reader takes it as no usable mapping.
    #[serde(other)]
    Invalid,
}

/// `chat.watch`: register the channel and begin its snapshot. Also the request to resync after a status asks for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatWatchReq {
    pub target: ChatTarget,
}

/// Where a watched channel stands (the watch model's terms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatWatchState {
    Needed,
    Pending,
    Complete,
    /// A state added later: a reader takes it as `needed` (a snapshot is owed).
    #[serde(other)]
    Other,
}

/// Reply to `chat.watch`. The messages come by `chat.history` and `chat.changed`, never in this reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatWatchRes {
    pub state: ChatWatchState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatUnwatchReq {
    pub target: ChatTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatUnwatchRes {}

/// The upper bound of a history query: one of the two, never both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatUpper {
    /// Strictly older than.
    Before(Ts),
    /// At or older than.
    Until(Ts),
}

/// `chat.history`: one page of the channel, or of the thread whose parent is `thread`. `after` is exclusive. `limit` is
/// 1 to 20. `cursor` continues the page a reply named, and only under the same query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHistoryReq {
    pub target: ChatTarget,
    #[serde(default)]
    pub thread: Option<Ts>,
    #[serde(default)]
    pub after: Option<Ts>,
    #[serde(default)]
    pub upper: Option<ChatUpper>,
    #[serde(deserialize_with = "page_limit")]
    pub limit: u16,
    #[serde(default, deserialize_with = "cursor")]
    pub cursor: Option<String>,
}

fn page_limit<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    let n = u16::deserialize(d)?;
    if (1..=MAX_PAGE).contains(&n) { Ok(n) } else { Err(serde::de::Error::custom("limit is 1 to 20")) }
}

fn retry_wait<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(u64::deserialize(d)?.min(MAX_RETRY_SECS))
}

fn cursor<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    struct Bounded(#[serde(deserialize_with = "within::<_, MAX_CURSOR>")] String);
    Ok(Option::<Bounded>::deserialize(d)?.map(|b| b.0))
}

/// Reply to `chat.history`. `next` is the cursor for the next page, absent on the last. `limited`: the history was cut
/// at the plan's horizon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHistoryRes {
    #[serde(deserialize_with = "at_most::<_, _, MAX_PAGE_RECORDS>")]
    pub records: Vec<ChatRecord>,
    #[serde(default, deserialize_with = "cursor", skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub limited: bool,
}

/// Whose post it is: the person at the window, or the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatPostKind {
    Person,
    Row,
}

/// `chat.post`: one message, submitted once and never resent. `name` is the author shown beside the app's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPostReq {
    pub target: ChatTarget,
    #[serde(default)]
    pub thread: Option<Ts>,
    pub kind: ChatPostKind,
    #[serde(deserialize_with = "within::<_, MAX_NAME>")]
    pub name: String,
    #[serde(deserialize_with = "within::<_, MAX_TEXT>")]
    pub text: String,
}

/// Reply to `chat.post`, by `outcome`. `refused`: definitely not posted. `limited`: not posted, and `retry_after_secs`
/// is how long to wait (`assumed` when the wait is a guess). `unknown`: the post may exist, so read history, never resend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ChatPostRes {
    Posted { ts: Ts },
    Refused { code: ChatCode },
    Limited {
        #[serde(deserialize_with = "retry_wait")]
        retry_after_secs: u64,
        assumed: bool,
    },
    /// Also what a reader takes an outcome added later for: the post may exist, so it is never resent.
    #[serde(other)]
    Unknown,
}

/// `chat.changed` evt: messages put or deleted in a watched channel, at most a page of each. A put is a new message or
/// the new content of one held under the same identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatChangedEvt {
    #[serde(deserialize_with = "within::<_, MAX_ID>")]
    pub workspace: String,
    pub channel: ChatChannel,
    #[serde(deserialize_with = "at_most::<_, _, MAX_PAGE_RECORDS>")]
    pub put: Vec<ChatRecord>,
    #[serde(deserialize_with = "at_most::<_, _, MAX_PAGE_RECORDS>")]
    pub deleted: Vec<Ts>,
}

/// The chat service's phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatService {
    Running,
    Stopping,
    Stopped,
    /// A phase added later: a reader treats it as not running and resyncs every watch.
    #[serde(other)]
    Other,
}

/// `chat.status` evt: the service's phase, whether a gap is open, and the watched channels that need a new snapshot
/// (answer with `chat.watch`). This is how a watch learns of a loss; `bus.lost` stays the event bus's own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatStatusEvt {
    pub service: ChatService,
    pub gap: bool,
    #[serde(deserialize_with = "at_most::<_, _, MAX_WATCHED>")]
    pub resync: Vec<ChatChannel>,
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
