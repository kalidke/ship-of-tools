// the project chat's operations and events (B2d): what the window and the daemon say about one watched channel

use super::*;
use crate::chat::{an_id, at_most, is_id, within, ChatRecord, Ts, MAX_ID, MAX_MENTIONS, MAX_MIME, MAX_NAME, MAX_TEXT};

/// The most messages a history page asks for or holds.
pub const MAX_PAGE: u16 = 20;
/// The most records one page, or one `chat.changed` event, carries: a page and a thread's parent.
pub const MAX_PAGE_RECORDS: usize = MAX_PAGE as usize + 1;
/// A history cursor; the daemon binds it to its whole query.
pub const MAX_CURSOR: usize = 1024;
/// The most channels watched at once (the watch model's bound), so the most a status can name.
pub const MAX_WATCHED: usize = 32;
/// The most Slack workspaces a hub serves, so the most secrets files its load opens.
pub const MAX_WORKSPACES: usize = 8;
/// The longest wait a `limited` post reply can name, in seconds.
pub const MAX_RETRY_SECS: u64 = 3600;
/// The most channels a `chat.channels` reply lists (the daemon's list is cut at this bound and says `more`).
pub const MAX_LISTED: usize = 1000;
/// A Slack token's longest form on the wire.
pub const MAX_TOKEN: usize = 512;
/// The largest file the chat posts or a picture it opens, in bytes: one copy of the number, for the daemon's read, the
/// upload and a reply's blob.
pub const MAX_CHAT_FILE: usize = 16 * 1024 * 1024;
/// The largest thumbnail a `chat.file` reply carries for the `thumb` variant, in bytes.
pub const MAX_THUMB: usize = 256 * 1024;
/// The most messages one `chat.read` asks Slack for, and so the most lines of a channel's read (a thread's adds its first
/// message above them).
pub const MAX_CHAT_READ: u16 = 50;
/// The most bytes of one line of a `chat.read` reply.
pub const MAX_READ_LINE: usize = 4096;
/// A `chat.post_file` node id (`files:<rel>`): the project-relative name of the file to post.
pub const MAX_NODE_ID: usize = 4096;
/// The most channels a `several_channels` refusal names.
pub const MAX_SEVERAL: usize = 4;
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
    /// Slack refused the bot token a `chat.connect` named.
    BotTokenRefused,
    /// Slack refused the app-level token a `chat.connect` named.
    AppTokenRefused,
    /// Slack refused the user token a `chat.connect` named.
    UserTokenRefused,
    /// The hub holds no Slack token that posts as the person (never held, or Slack refused it and the chat forgot it): a
    /// post's refusal, which brings the window's Connect step. `chat.resolve` says nothing of it.
    NoUserToken,
    /// The person is not in the channel they posted to.
    NotAMember,
    /// The token belongs to another Slack workspace than the one the chat is set up for: only a daemon that serves one
    /// workspace sends it.
    OtherWorkspace,
    /// A `chat.connect` of a new workspace when the hub already holds `MAX_WORKSPACES`' secrets files.
    TooManyWorkspaces,
    /// The channel a `chat.pair` named is not one of its workspace's (Slack's `channel_not_found` to that workspace's app,
    /// which is also Slack's answer for a private channel the app is not in, so the text says both).
    NoSuchChannel,
    /// The app is not in the channel a `chat.pair` named, and could not join it; or Slack refused a read of a mapped channel
    /// (a watch's snapshot, a page, a thread) because the app is not in it (`not_in_channel`, or `channel_not_found`).
    NotInChannel,
    /// Another project's folder already maps the channel a `chat.pair` named.
    ChannelTaken,
    /// The chat's files could not be written (or the one there is was refused by the load).
    NotSaved,
    /// A file to post is not a readable regular file in the row's folder (or is the chat's own configuration).
    NotAFile,
    /// A file is larger than the chat carries.
    TooLarge,
    /// A file is not one of the pictures the chat shows, or Slack answered something that is not a picture.
    NotAnImage,
    /// A `chat.connect` without `replace_app` whose tokens are another Slack app's than the one its workspace runs with
    /// now, or not known to be that app's (their app or the running one not told): nothing was saved, and the refusal's
    /// body says what a window asks with (`ChatOtherApp`).
    OtherApp,
    /// A `chat.connect`, with `replace_app` or without, whose app-level token is another Slack app's than its bot token's:
    /// nothing was saved. A window sends the person back to the app-level token's step, the other two kept, and shows the
    /// text there and on its status line.
    MixedApps,
    /// A `chat.delete` Slack refused with `cant_delete_message`: it would not let the hub's Slack app delete that message
    /// (the app's own, or not: the words claim no more).
    NotOwnMessage,
    /// A session's `chat.row_pair` of a project that already has a channel: nothing was changed, and the refusal's body is
    /// the folder's pairing (`ChatPairing`), so the session can say what it has and that it must unpair it first.
    AlreadyPaired,
    /// A session's `chat.row_pair` named a channel by a name that more than one channel has: nothing was joined or changed,
    /// and the refusal's body names them (`ChatSeveralChannels`).
    SeveralChannels,
    Failed,
    #[serde(other)]
    Other,
}

impl ChatCode {
    /// The one fixed text of a code: the `error` of a refusal, and what a window shows for it.
    pub fn text(self) -> &'static str {
        match self {
            ChatCode::Unavailable => "the chat is not available here",
            ChatCode::Unmapped => "the session's project has no channel",
            ChatCode::StaleRow => "the session is gone or was replaced",
            ChatCode::StaleMapping => "the session's project now has another channel",
            ChatCode::BadRequest => "the request is not valid",
            ChatCode::TooManyWatches => "too many channels are being followed at once",
            ChatCode::Limited => "Slack asked for a pause; try again in a moment",
            ChatCode::Busy => "the chat is busy; try again in a moment",
            ChatCode::BotTokenRefused => "Slack refused the \"Bot User OAuth Token\"",
            ChatCode::AppTokenRefused => "Slack refused the \"App-Level Token\"",
            ChatCode::UserTokenRefused => "Slack refused the \"User OAuth Token\"",
            ChatCode::NoUserToken => "no Slack code that posts as you is saved; open the channel list (Ctrl+Shift+k, or Cmd+Shift+k on a Mac) and choose its last entry, Connect a Slack workspace (Connect Slack again on an older computer)",
            ChatCode::NotAMember => "you are not in that channel in Slack; join it there first",
            ChatCode::OtherWorkspace => "these Slack codes are for another Slack workspace, and Ship of Tools on the computer that runs your sessions is too old to connect more than one: whoever runs Ship of Tools there can update it",
            ChatCode::TooManyWorkspaces => "the computer that runs your sessions already holds 8 Slack workspaces, the most it can: to add another, remove one workspace's file chat-secrets-<id>.toml (<id> is that workspace's ID) from Ship of Tools' settings folder there, ~/.config/sot on Linux and macOS or %LOCALAPPDATA%\\sot\\config on Windows",
            ChatCode::NoSuchChannel => "that channel is not in this Slack workspace, or it is a private channel your Slack app is not in",
            ChatCode::NotInChannel => "your Slack app is not in that channel",
            ChatCode::ChannelTaken => "another project already uses that channel",
            ChatCode::NotSaved => "the chat settings could not be saved",
            ChatCode::NotAFile => "the file is not a readable file in the project's folder",
            ChatCode::TooLarge => "the file is larger than the chat carries",
            ChatCode::NotAnImage => "that file is not a picture the chat can show",
            ChatCode::OtherApp => "these Slack codes may belong to a different Slack app than the one this Slack workspace is connected with now; nothing was changed",
            ChatCode::MixedApps => "the \"App-Level Token\" belongs to a different Slack app than the \"Bot User OAuth Token\", so nothing was changed; all three codes must come from one Slack app",
            ChatCode::NotOwnMessage => "Slack would not let this project's Slack app delete that message (a person's or another app's message stays)",
            ChatCode::AlreadyPaired => "the project already has a channel; unpair it first",
            ChatCode::SeveralChannels => "several channels have that name; name one by its ID",
            ChatCode::Failed | ChatCode::Other => "the chat request failed",
        }
    }
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

/// Reply to `chat.resolve`. `delivers`: mail may be delivered for the row (a `mail_after` is set). `name`: the channel's
/// name as the daemon's cache holds it (a resolve makes no Slack call); absent, empty, too long or
/// not a string, a reader shows the id. `workspace_name`: the mapping's Slack workspace's name, sent only while the hub
/// serves more than one workspace, and read as `name` is. `workspace_tag`: the workspace's short name (the first label of its Slack address, else its name), sent whenever known and read as `name` is; an older daemon sends none and its window shows the older title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChatResolveRes {
    Mapped {
        #[serde(deserialize_with = "within::<_, MAX_ID>")]
        workspace: String,
        channel: ChatChannel,
        delivers: bool,
        #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
        workspace_name: Option<String>,
        #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
        workspace_tag: Option<String>,
    },
    Unmapped,
    /// The hub's chat is off for a reason a `chat.connect` fixes. `workspace`: the row's mapping's workspace, when it has
    /// one (the one whose connection failed); absent from an older daemon and for a row with no mapping.
    Unconfigured {
        reason: ChatUnconfigured,
        #[serde(default, deserialize_with = "named_id", skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    /// Also a state added later: a reader takes it as no usable mapping (an older window shows `unconfigured` as its
    /// no-configuration words).
    #[serde(other)]
    Invalid,
}

/// Why the hub's chat is off, when connecting fixes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatUnconfigured {
    /// No `chat.toml` or no `chat-secrets.toml`.
    NotConnected,
    /// The saved tokens are refused (by the load or by Slack), or Slack refused one while the chat ran.
    TokensRefused,
    /// The service failed to start.
    StartFailed,
    /// The app-level token is another Slack app's than the bot's: the chat ended rather than take that app's events.
    WrongApp,
    /// A reason added later.
    #[serde(other)]
    Other,
}

/// A Slack token on the wire: serialized as its string, at most `MAX_TOKEN` bytes, and never printed (`Debug` is
/// `<redacted>`; there is no `Display`), so a derived `Debug` of a request that holds one leaks no byte of it.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct ChatToken(String);

impl TryFrom<String> for ChatToken {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if s.len() <= MAX_TOKEN { Ok(ChatToken(s)) } else { Err(format!("more than {MAX_TOKEN} bytes")) }
    }
}

impl Serialize for ChatToken {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl std::fmt::Debug for ChatToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl ChatToken {
    /// The token's bytes, for the one place that sends or checks them.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// `chat.connect`: the three tokens of a Slack app (`xoxb-`, `xoxp-` and `xapp-`); the daemon checks them all before it
/// saves any. A request with no `user_token` (an older window's) decodes with `None` and is refused `bad_request`.
/// `replace_app`: the person chose to replace the Slack app the workspace runs with now by the one these tokens are for (the
/// window's answer to `other_app`); defaulted and left out when false, so an older window's connect is asked about, and an
/// older daemon, which ignores the field, replaces as it always did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatConnectReq {
    pub bot_token: ChatToken,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_token: Option<ChatToken>,
    pub app_token: ChatToken,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_app: bool,
}

/// The rest of a `chat.connect` refused `other_app`, beside its `error` and `code`: the workspace the tokens are for
/// (`workspace`, and `name`, its name as Slack gave it), the Slack app it runs with now (`app_id`, and `app_name`, the
/// name Slack gives that app's bot; both absent while the daemon does not know the running app), whether the daemon told
/// the tokens' app to be another than the running one (`new_app_known`; false: Slack did not say which app the tokens are
/// for, or which app runs), and the name Slack gives that app's bot (`new_app_name`, only when known). Each is
/// read as `chat.workspaces`' fields are, so a bad one is none (a flag false), never a reason to refuse the refusal, and
/// a bare refusal is the weaker claim, an app not told; an older reader ignores them all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatOtherApp {
    #[serde(default, deserialize_with = "named_id", skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "named_id", skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(default, deserialize_with = "flag", skip_serializing_if = "std::ops::Not::not")]
    pub new_app_known: bool,
    #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
    pub new_app_name: Option<String>,
}

/// A flag a reply gives for words: `true` alone is true; anything else is false.
fn flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_bool() == Some(true))
}

/// Reply to `chat.connect`: the Slack workspace the bot token belongs to, and its name as Slack gave it (absent from an
/// older daemon and when Slack gave none; read as a resolve's channel `name` is, so a bad one is no name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatConnectRes {
    #[serde(deserialize_with = "within::<_, MAX_ID>")]
    pub workspace: String,
    #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A mapped channel the running Slack app cannot read: its id, and its name as the app connected before had it, else as
/// Slack gave it (absent when neither was known; read as a resolve's `name` is).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatUnread {
    pub id: ChatChannel,
    #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// An entry's `unread`: a list is known (empty or not), of its readable entries the first `MAX_WATCHED`; absent, null or
/// any other value is not known.
fn unread_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<ChatUnread>>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Array(all) => Some(all.into_iter().filter_map(|v| serde_json::from_value(v).ok()).take(MAX_WATCHED).collect()),
        _ => None,
    })
}

/// `chat.channels`: the channels the picker offers, of `workspace` (absent: `chat.toml`'s own, as an older window asks).
/// A session's request carries `by`, who asks (checked as a `chat.row_pair`'s is); with no `workspace` it means the
/// asking project's own workspace, else `chat.toml`'s, and its reply says which and marks each taken channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatChannelsReq {
    #[serde(default, deserialize_with = "workspace_id", skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<ChatAsker>,
}

/// An optional workspace a request names: when present, an id within `MAX_ID`; absent or null is none.
fn workspace_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    struct Id(#[serde(deserialize_with = "an_id")] String);
    Ok(Option::<Id>::deserialize(d)?.map(|i| i.0))
}

/// An id a reply names for words, a choice or an address (a workspace, a Slack app): one that is no id reads as none,
/// never a reason to refuse the reply it rides on.
fn named_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) if is_id(&s) => Some(s),
        _ => None,
    })
}

/// `chat.workspaces`: the Slack workspaces the hub is connected to, a read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatWorkspacesReq {}

/// One connected workspace: its id, its name as Slack last gave it (a reader shows the id without one), whether its chat
/// runs now, and, while it runs, of the Slack app it is connected to: `app_name`, the name Slack gives that app's bot
/// (read as `name` is), `app_id`, the app's id (read as an id: one that is not is none), and `unread`, the workspace's
/// mapped channels that app cannot read, once known (a list, empty when it reads them all). Each is absent while not
/// known, while the workspace does not run, and from an older daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatWorkspaceInfo {
    #[serde(deserialize_with = "an_id")]
    pub id: String,
    #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub running: bool,
    #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(default, deserialize_with = "named_id", skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, deserialize_with = "unread_list", skip_serializing_if = "Option::is_none")]
    pub unread: Option<Vec<ChatUnread>>,
}

/// Reply to `chat.workspaces`: `chat.toml`'s workspace (the one a request naming none means) and every connected
/// workspace, ordered by name, the id breaking a tie. A reader keeps the first `MAX_WORKSPACES` entries it can read and
/// skips the rest, never refusing the reply, so a later daemon's longer list or a malformed entry costs only entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatWorkspacesRes {
    #[serde(deserialize_with = "an_id")]
    pub default: String,
    #[serde(deserialize_with = "workspace_list")]
    pub workspaces: Vec<ChatWorkspaceInfo>,
}

/// The readable entries of a list, the first `MAX_WORKSPACES` of them: an entry that does not decode is skipped.
fn workspace_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ChatWorkspaceInfo>, D::Error> {
    let all = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(all.into_iter().filter_map(|v| serde_json::from_value(v).ok()).take(MAX_WORKSPACES).collect())
}

/// One listed channel: its id, its name as the daemon cleaned it, whether it is private, whether the app is in it, and,
/// in a session's list only, whether a project of this computer is paired with it (`taken`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatChannelInfo {
    pub id: ChatChannel,
    #[serde(deserialize_with = "within::<_, MAX_NAME>")]
    pub name: String,
    pub private: bool,
    pub member: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub taken: bool,
}

/// Reply to `chat.channels`, at most `MAX_LISTED` entries; `more`: the list was cut there. A session's reply also names
/// the `workspace` the list is of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatChannelsRes {
    #[serde(deserialize_with = "at_most::<_, _, MAX_LISTED>")]
    pub channels: Vec<ChatChannelInfo>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
    #[serde(default, deserialize_with = "workspace_id", skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// `chat.pair`: map the row's folder to `channel` of `workspace` (absent: `chat.toml`'s own, as an older window asks), or,
/// with no channel (absent or null), unmap it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPairReq {
    #[serde(deserialize_with = "within::<_, MAX_ROW>")]
    pub row: String,
    #[serde(default, deserialize_with = "workspace_id", skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default)]
    pub channel: Option<ChatChannel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPairRes {}

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

/// A name that is empty, longer than `MAX_NAME` or not a string reads as no name: a cosmetic field is never a reason to
/// refuse the mapping it rides on.
fn channel_name<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) if !s.is_empty() && s.len() <= MAX_NAME => Some(s),
        _ => None,
    })
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

/// A name after `@` in a post that the daemon left as text, and why. At most `MAX_MENTIONS` on a reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatUnlinked {
    #[serde(deserialize_with = "within::<_, MAX_NAME>")]
    pub name: String,
    pub why: ChatUnlinkedWhy,
    /// For `several_members`, the names Slack shows for the members the name could be, at most `MAX_CANDIDATES`; an
    /// empty last name means there were more. Read leniently.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient_candidates")]
    pub candidates: Vec<String>,
}

/// The most candidates a `several_members` entry names, and one more place for the empty "and more" name.
pub const MAX_CANDIDATES: usize = 4;

/// Names read as a cosmetic list: an entry that is no string or is past `MAX_NAME` is dropped, at most `max` kept, and a
/// field that is no list is none.
fn lenient_names<'de, D: serde::Deserializer<'de>>(d: D, max: usize) -> Result<Vec<String>, D::Error> {
    let entries = match serde_json::Value::deserialize(d)? {
        serde_json::Value::Array(entries) => entries,
        _ => Vec::new(),
    };
    Ok(entries.into_iter().filter_map(|e| e.as_str().filter(|n| n.len() <= MAX_NAME).map(str::to_string)).take(max).collect())
}

fn lenient_candidates<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    lenient_names(d, MAX_CANDIDATES + 1)
}

/// `deserialize_with` of a `posted` reply's `mentioned`: the names Slack shows for the members the post mentions, in
/// the text's order, at most `MAX_MENTIONS` (an empty last name means more), read as `lenient_names` reads.
fn lenient_mentioned<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    lenient_names(d, MAX_MENTIONS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatUnlinkedWhy {
    NoMember,
    SeveralMembers,
    MembersUnread,
    TooManyMembers,
    /// `@channel`, `@here` or `@everyone` in a row's post: an agent never notifies a whole channel.
    ChannelWide,
    /// Linked, the text would pass `MAX_TEXT`.
    TooLong,
    /// The last entry when more names were left as text than a reply names (its `name` is empty).
    More,
    /// A reason added later.
    #[serde(other)]
    Other,
}

/// `deserialize_with` of a `posted` reply's `unlinked`, which is cosmetic: an entry that is no object or does not decode within its bounds,
/// one past `MAX_MENTIONS`, or a field that is no list is dropped, never a reason to read a made post as not made.
fn lenient_unlinked<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ChatUnlinked>, D::Error> {
    let entries = match serde_json::Value::deserialize(d)? {
        serde_json::Value::Array(entries) => entries,
        _ => Vec::new(),
    };
    Ok(entries.into_iter().filter(|e| e.is_object()).filter_map(|e| serde_json::from_value(e).ok()).take(MAX_MENTIONS).collect())
}

/// What a `posted` reply tells of its `@` names, the one form the window's status line and `sot-fe chat post` end their
/// words with: `; mentioned <n>: <entries>` and `; not linked: <name> (<reason>)`, a `several_members` entry as `<name>
/// (several members, <n>: <entries>)`. The count is the number of people a list names, `<n> and more` when an empty
/// last name says there were more (the list then ends `, and more`); entries are printed as the daemon made them
/// (chat guarantee 35: a double quote is always structure), joined by `, `. Empty when there is nothing to tell.
pub fn told_words(mentioned: &[String], unlinked: &[ChatUnlinked]) -> String {
    let list = |names: &[String]| {
        let named: Vec<&str> = names.iter().map(String::as_str).filter(|n| !n.is_empty()).collect();
        let more = if named.len() < names.len() { " and more" } else { "" };
        format!("{}{more}: {}{}", named.len(), named.join(", "), if more.is_empty() { "" } else { ", and more" })
    };
    let entry = |u: &ChatUnlinked| match u.why {
        ChatUnlinkedWhy::More => u.why.text().to_string(),
        _ if !u.candidates.is_empty() => format!("{} ({}, {})", u.name, u.why.text(), list(&u.candidates)),
        _ => format!("{} ({})", u.name, u.why.text()),
    };
    let mut out = String::new();
    if !mentioned.is_empty() {
        out += &format!("; mentioned {}", list(mentioned));
    }
    if !unlinked.is_empty() {
        out += &format!("; not linked: {}", unlinked.iter().map(entry).collect::<Vec<_>>().join(", "));
    }
    out
}

impl ChatUnlinkedWhy {
    /// The one fixed text of a reason, as the window and `sot-fe chat post` show it.
    pub fn text(self) -> &'static str {
        match self {
            ChatUnlinkedWhy::NoMember => "no member",
            ChatUnlinkedWhy::SeveralMembers => "several members",
            ChatUnlinkedWhy::MembersUnread => "the member list could not be read",
            ChatUnlinkedWhy::TooManyMembers => "the member list is too large",
            ChatUnlinkedWhy::ChannelWide => "a session never notifies a whole channel",
            ChatUnlinkedWhy::TooLong => "the post would be too long",
            ChatUnlinkedWhy::More => "and more",
            ChatUnlinkedWhy::Other => "not linked",
        }
    }
}

/// Reply to `chat.post`, by `outcome`. `refused`: definitely not posted. `limited`: not posted, and `retry_after_secs`
/// is how long to wait (`assumed` when the wait is a guess). `unknown`: the post may exist, so read history, never resend.
/// `posted` names the members the post mentions (`mentioned`) and the `@` names the daemon left as text (`unlinked`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ChatPostRes {
    Posted {
        ts: Ts,
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient_unlinked")]
        unlinked: Vec<ChatUnlinked>,
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient_mentioned")]
        mentioned: Vec<String>,
    },
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

/// `chat.post_file`: one file of the row's project shared once into the channel, never resent. `node_id` is the file's
/// `files:<rel>` id, composed by the daemon beneath the row's root. `name` is the author shown in the file's label. A
/// `comment` rides after the label. The bytes are never in a request: the daemon reads the file itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPostFileReq {
    pub target: ChatTarget,
    #[serde(default)]
    pub thread: Option<Ts>,
    pub kind: ChatPostKind,
    #[serde(deserialize_with = "within::<_, MAX_NAME>")]
    pub name: String,
    #[serde(deserialize_with = "within::<_, MAX_NODE_ID>")]
    pub node_id: String,
    #[serde(default, deserialize_with = "comment")]
    pub comment: Option<String>,
}

fn comment<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    struct Bounded(#[serde(deserialize_with = "within::<_, MAX_TEXT>")] String);
    Ok(Option::<Bounded>::deserialize(d)?.map(|b| b.0))
}

/// Reply to `chat.post_file`, by `outcome`, as a post's: `refused` shared nothing; `limited` is unsent; `unknown` means
/// the share may exist and is never resent. `posted` names the file Slack made and, when Slack's answer carried it, the
/// `ts` of the message that shares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ChatPostFileRes {
    Posted {
        #[serde(deserialize_with = "an_id")]
        file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ts: Option<Ts>,
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient_unlinked")]
        unlinked: Vec<ChatUnlinked>,
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient_mentioned")]
        mentioned: Vec<String>,
    },
    Refused { code: ChatCode },
    Limited {
        #[serde(deserialize_with = "retry_wait")]
        retry_after_secs: u64,
        assumed: bool,
    },
    /// Also what a reader takes an outcome added later for: the share may exist, so it is never resent.
    #[serde(other)]
    Unknown,
}

/// `chat.delete`: the message `ts` of the row's own channel, deleted once under the Slack app's own token and never
/// resent. A row's request only (a window's is `bad_request`): `name` is the row's bound handle, checked as a row's post's
/// is. There is no kind and no channel but the target's, which the daemon rechecks against the row's mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDeleteReq {
    pub target: ChatTarget,
    #[serde(deserialize_with = "within::<_, MAX_NAME>")]
    pub name: String,
    pub ts: Ts,
}

/// Reply to `chat.delete`, by `outcome`. `deleted`: the message is gone (a file it shared is not touched). `gone`: Slack
/// has no such message (also what a second ask after a delete reads). `refused`: definitely nothing changed
/// (`not_own_message`: Slack would not let the app delete that message). `limited`: nothing was deleted, and `retry_after_secs` is how
/// long Slack asked to wait (`assumed` when the wait is a guess). `unknown`: the message may be deleted, so ask again,
/// which says what is true now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ChatDeleteRes {
    Deleted,
    Gone,
    Refused { code: ChatCode },
    Limited {
        #[serde(deserialize_with = "retry_wait")]
        retry_after_secs: u64,
        assumed: bool,
    },
    /// Also what a reader takes an outcome added later for.
    #[serde(other)]
    Unknown,
}

/// `chat.read`: a session's read of its own project's channel, or of one thread of it, as the daemon's words. `name` is the
/// row's bound handle, checked as a delete's is (attribution among one account's programs, not authentication). `thread`
/// reads that thread; `before` reads the messages Slack lists strictly before it (absent: before now). The channel is the
/// target's, which the daemon rechecks against the row's mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatReadReq {
    pub target: ChatTarget,
    #[serde(deserialize_with = "some_within::<_, MAX_NAME>")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<Ts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Ts>,
}

/// Reply to `chat.read`: one line a message, oldest first, built by the daemon so every word of channel text is escaped
/// once. `more` is the oldest message the read held, when older ones exist (the next read's `before`); `limited`: Slack's
/// plan hides older messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatReadRes {
    #[serde(deserialize_with = "read_lines")]
    pub lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub more: Option<Ts>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub limited: bool,
}

/// At most `MAX_CHAT_READ` lines and the thread's first message, each at most `MAX_READ_LINE` bytes.
fn read_lines<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    struct Line(#[serde(deserialize_with = "within::<_, MAX_READ_LINE>")] String);
    let lines = at_most::<_, Line, { MAX_CHAT_READ as usize + 1 }>(d)?;
    Ok(lines.into_iter().map(|Line(l)| l).collect())
}

/// The session a `chat.row_pair` comes from: the row it runs in and the handle bound to that row, checked as a row post's
/// name is (attribution among one account's programs, not authentication).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAsker {
    #[serde(deserialize_with = "within::<_, MAX_ROW>")]
    pub row: String,
    #[serde(deserialize_with = "some_within::<_, MAX_NAME>")]
    pub name: String,
}

/// `chat.row_pair`: pair the folder of `row` (an id or a slug) with `channel`, an id or a name, in `workspace` (absent:
/// `chat.toml`'s), or, with `channel` null, unpair it. `channel` is a required key: a request that lost it is refused,
/// never read as an unpairing, since an older daemon that drops a field it does not know would read it so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRowPairReq {
    pub by: ChatAsker,
    #[serde(deserialize_with = "within::<_, MAX_ROW>")]
    pub row: String,
    #[serde(default, deserialize_with = "workspace_id", skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(deserialize_with = "channel_given")]
    pub channel: Option<String>,
}

/// A string of 1 to `N` bytes.
fn some_within<'de, D: serde::Deserializer<'de>, const N: usize>(d: D) -> Result<String, D::Error> {
    let s = within::<_, N>(d)?;
    if s.is_empty() {
        return Err(serde::de::Error::custom("empty"));
    }
    Ok(s)
}

/// A channel as a session gives it: null is none, else 1 to `MAX_NAME` bytes with no control character.
fn channel_given<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    match Option::<String>::deserialize(d)? {
        Some(s) if s.is_empty() || s.len() > MAX_NAME || s.chars().any(char::is_control) => Err(serde::de::Error::custom("not a channel")),
        given => Ok(given),
    }
}

/// How a `chat.row_pair` ended: `paired` and `unpaired` changed the file, `unchanged` left it as it was. A reader takes an
/// outcome added later for `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatPairOutcome {
    Paired,
    Unpaired,
    Unchanged,
    #[serde(other)]
    Other,
}

/// A folder's pairing: its workspace and that workspace's short name, the channel and its name as the daemon's cache holds
/// them (each read as a resolve's is: one that is no name is none), and whether new posts there are mail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPairing {
    #[serde(deserialize_with = "within::<_, MAX_ID>")]
    pub workspace: String,
    #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
    pub workspace_tag: Option<String>,
    pub channel: ChatChannel,
    #[serde(default, deserialize_with = "channel_name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delivers: bool,
}

/// The body of a `several_channels` refusal: the ids of the channels that have the name given, at most `MAX_SEVERAL`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSeveralChannels {
    #[serde(deserialize_with = "at_most::<_, _, MAX_SEVERAL>")]
    pub channels: Vec<ChatChannel>,
}

/// Reply to `chat.row_pair`: the folder (the hub's canonical spelling, for the asking session alone), and its pairing now
/// (`paired`, `unchanged`) or the one removed (`unpaired`); none for an `unchanged` unpairing of a folder with no channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRowPairRes {
    pub outcome: ChatPairOutcome,
    #[serde(deserialize_with = "within::<_, MAX_NODE_ID>")]
    pub folder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing: Option<ChatPairing>,
}

/// Which bytes of a channel's picture `chat.file` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatFileVariant {
    /// Slack's small rendition (at most `MAX_THUMB` bytes).
    Thumb,
    /// The file as posted (at most `MAX_CHAT_FILE` bytes).
    Original,
}

/// `chat.file`: the bytes of one picture shared in the target's channel. `file` is a record's file id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatFileReq {
    pub target: ChatTarget,
    #[serde(deserialize_with = "an_id")]
    pub file: String,
    pub variant: ChatFileVariant,
}

/// Reply to `chat.file`. The picture's bytes are the frame's trailing blob, not this JSON. `blob` is the codec's
/// descriptor (`len` = the byte count, at most `MAX_CHAT_FILE`; REQUIRED, or the reader takes the next envelope for
/// bytes). The codec reads the blob before this decode, so the decoder's bound refuses the reply but not the read: the
/// reader checks `len` against its request's variant before it asks (fe-net does).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatFileRes {
    #[serde(deserialize_with = "within::<_, MAX_MIME>")]
    pub mime: String,
    #[serde(deserialize_with = "blob_within")]
    pub blob: BlobDescriptor,
}

fn blob_within<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BlobDescriptor, D::Error> {
    let b = BlobDescriptor::deserialize(d)?;
    if b.len > MAX_CHAT_FILE as u64 {
        return Err(serde::de::Error::custom("more than the chat's file bound"));
    }
    Ok(b)
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
