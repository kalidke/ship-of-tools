//! The Slack Web API calls the first chat stage needs: `auth.test` (and `identify`, the setup's, before a workspace is
//! configured), `bots.info` for the bot's app, channel history and thread replies (paged), `chat.postMessage` under a
//! custom author, a file shared into a channel (Slack's three upload steps), the channel picker's calls (the channel
//! list, one channel's facts, one join), author names through a bounded cache, and the member list a post's `@` names are
//! linked from (`mention`). Every call goes out through `http`
//! and comes back as the wire record through `normalize`; there is no second record and no second HTTP path.
//! No socket, no file method, no configuration: the caller hands in the `Secret` and a file's bytes.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Url;
use serde_json::{json, Value};
use sot_protocol::chat::{escape_text, post_text_fits, text_segments, ChatName, ChatRecord, Origin, TextSegment, Ts, MAX_TEXT, PICTURE_MIMES};
use sot_protocol::ops::{ChatUnlinked, ChatUnlinkedWhy};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use crate::http::{Http, HttpError, Request, Response, Secret, MAX_BODY, MAX_SCAN_BODY};
use crate::mention::{link, Index, Linked, Member, Roster};
use crate::normalize::{clean, normalize, ts_key, valid_id, Context, Normalized, MAX_FILENAME, MAX_MENTIONS, MAX_NAME};
use crate::service::{End, BOT_TOKEN_REFUSED};
use crate::watch::MAX_CHANNELS;

/// A read (`auth.test`, history, replies, `users.info`, `conversations.info`): Slack answers in well under a second, so
/// fifteen seconds covers a slow link and still frees a drawer waiting on a stalled page.
pub const READ_DEADLINE: Duration = Duration::from_secs(15);
/// A post is never resent, so its wait is longer than a read's: giving up early turns an acknowledgement still on its
/// way into `Unknown` for the person to resolve.
pub const POST_DEADLINE: Duration = Duration::from_secs(30);
/// A file's bytes on the wire (a post's upload): up to `MAX_CHAT_FILE` on a slow link.
pub const TRANSFER_DEADLINE: Duration = Duration::from_secs(60);
/// Transfers at once, per `Transfers` (one daemon's, shared by every workspace's `Api`): the next is refused `busy` at
/// once, so at most this many files' bytes are held for the network.
pub const MAX_TRANSFERS: usize = 2;
/// A URL Slack answers (an upload's, a file's): a longer one is `malformed`.
const MAX_URL: usize = 2048;
/// All the name lookups for one page together: names are decoration, so the page is shown with ids after this.
pub const NAMES_DEADLINE: Duration = Duration::from_secs(10);
pub use sot_protocol::ops::{MAX_CHAT_FILE, MAX_CURSOR, MAX_LISTED, MAX_PAGE, MAX_THUMB};
/// The most messages one scan asks for: Slack's documented maximum for a history read.
pub const MAX_SCAN: u16 = 999;
/// Author names kept; the oldest entry leaves first.
pub const MAX_NAMES: usize = 1024;
/// Reads wait this long after a rate limit whose retry header is missing or invalid.
pub const ASSUMED_RETRY: Duration = Duration::from_secs(30);
/// A hostile or mistaken retry header cannot silence reads longer than this.
pub const MAX_RETRY: Duration = Duration::from_secs(sot_protocol::ops::MAX_RETRY_SECS);

/// The channel list's page size, and the pages one list follows: at most wire's `MAX_LISTED` channels, the rest unlisted.
const LIST_PAGE: usize = 200;
const MAX_LIST_PAGES: usize = 5;
const _: () = assert!(LIST_PAGE * MAX_LIST_PAGES == MAX_LISTED);
/// The member list's page size and the pages one walk reads: a workspace of more than `MAX_MEMBERS` people, bots and
/// deactivated accounts links no name. Each page is bounded by `MEMBERS_BODY`.
const MEMBER_PAGE: usize = 200;
const MAX_MEMBER_PAGES: usize = 10;
pub const MAX_MEMBERS: usize = MEMBER_PAGE * MAX_MEMBER_PAGES;
const MEMBERS_BODY: usize = 4 << 20;
/// How long a member list read serves posts before the next post that names someone reads it again.
pub const MEMBERS_FRESH: Duration = Duration::from_secs(600);

const BASE: &str = "https://slack.com/api/";
const JSON: &str = "application/json; charset=utf-8";

/// Why a call failed or was refused. Fixed codes: Slack's own error text and bodies never reach a caller; its error
/// code does, only when it is `[a-z0-9_.]{1,64}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    /// The HTTP core failed (see `HttpError`).
    Http(HttpError),
    /// Slack answered `ok: false` with this code.
    Slack(String),
    /// A status that is neither success nor a rate limit.
    Status(u16),
    /// Reads are not eligible for `retry_after`. `assumed`: the retry header was missing or invalid.
    Limited {
        retry_after: Duration,
        assumed: bool,
    },
    /// The answer is not the shape the call needs, or a message in it cannot be represented.
    Malformed,
    /// An input of the caller's is refused before anything is sent (an id, limit, name or text).
    BadRequest,
    /// A bound is not exactly `<seconds>.<six digits>`, or both `before` and `until` were given.
    BadBound,
    /// History needs the bot identity from `auth_test` first.
    NoIdentity,
    /// `auth.test` named another workspace than the one configured.
    WrongWorkspace,
    /// A continuation does not belong to this query, or is not one this client made.
    Altered,
    /// Slack returned the cursor the client had just sent.
    Repeated,
    /// `has_more: true` with no cursor: the page is incomplete, never the last.
    Missing,
    /// A wrapped continuation would pass `MAX_CURSOR`.
    TooLarge,
    /// The owner closed this client (its service stopped): nothing is sent.
    Closed,
    /// A person's post, and no user token is held: nothing is sent, and the bot's token is never used in its place.
    NoPersonToken,
}

impl ApiError {
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::Http(_) => "http",
            ApiError::Slack(_) => "slack",
            ApiError::Status(_) => "status",
            ApiError::Limited { .. } => "limited",
            ApiError::Malformed => "malformed",
            ApiError::BadRequest => "bad_request",
            ApiError::BadBound => "bad_bound",
            ApiError::NoIdentity => "no_identity",
            ApiError::WrongWorkspace => "wrong_workspace",
            ApiError::Altered => "altered_continuation",
            ApiError::Repeated => "repeated_continuation",
            ApiError::Missing => "missing_continuation",
            ApiError::TooLarge => "continuation_too_large",
            ApiError::Closed => "closed",
            ApiError::NoPersonToken => "no_person_token",
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Http(e) => write!(f, "http: {e}"),
            ApiError::Slack(code) => write!(f, "slack: {code}"),
            ApiError::Status(s) => write!(f, "status: {s}"),
            ApiError::Limited { retry_after, .. } => {
                write!(f, "limited: retry in {}s", retry_after.as_secs())
            }
            other => f.write_str(other.code()),
        }
    }
}

impl std::error::Error for ApiError {}

/// The bot's identity from `auth.test`: normalization decides which messages are this app's own by it alone. `app_id`
/// is the bot's app as `bots.info` names it (`Api::bot_app`, set by the service's start), which the listener holds
/// every frame to; `None` while unknown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub user_id: String,
    pub bot_id: Option<String>,
    pub app_id: Option<String>,
}

/// The bot's app as `bots.info` gives it: the app's id, and the name Slack gives the bot (`bot.name`, read as a
/// workspace's name is; none when absent or unusable). Whether that name is the app's in the person's list of Slack apps
/// or the bot's own display name is not measured; an app made from the setup's manifest gives both the same name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotApp {
    pub id: String,
    pub name: Option<String>,
}

/// A channel as `conversations.list` and `conversations.info` give it. `private` is true unless Slack says the channel is
/// not private, and `member` false unless it says the bot is in it, so a reply that does not say never offers more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub private: bool,
    pub member: bool,
}

/// The channels the bot can see, at most `MAX_LISTED`; `more` when Slack had a page left at the bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelList {
    pub channels: Vec<Channel>,
    pub more: bool,
}

/// How a join ended. It was submitted at most once, whatever the outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinOutcome {
    /// Slack acknowledged it, or said the bot was already in the channel.
    Joined,
    /// Definitely not joined: Slack's rejection (its code), or the request never left (`busy`, `invalid`, `destination`).
    Refused(String),
    /// No usable acknowledgement (a rate limit included): the bot may or may not be in. Never resent; the caller reads
    /// the channel.
    Unknown,
}

/// The upper bound of a query. One variant at most, so `before` and `until` cannot both be asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Upper {
    /// Strictly older than.
    Before(Ts),
    /// At or older than.
    Until(Ts),
}

/// Query bounds, each a wire `Ts`: exactly six fraction digits, or the bound is refused.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Bounds {
    after: Option<Ts>,
    upper: Option<Upper>,
}

impl Bounds {
    /// `after` is exclusive. At most one of `before` (exclusive) and `until` (inclusive).
    pub fn new(
        after: Option<&str>,
        before: Option<&str>,
        until: Option<&str>,
    ) -> Result<Bounds, ApiError> {
        let ts = |s: &str| Ts::parse(s).map_err(|_| ApiError::BadBound);
        let upper = match (before, until) {
            (None, None) => None,
            (Some(b), None) => Some(Upper::Before(ts(b)?)),
            (None, Some(u)) => Some(Upper::Until(ts(u)?)),
            (Some(_), Some(_)) => return Err(ApiError::BadBound),
        };
        Ok(Bounds {
            after: after.map(ts).transpose()?,
            upper,
        })
    }

    fn admits(&self, ts: &Ts) -> bool {
        let at = ts_key(ts);
        self.after.as_ref().is_none_or(|a| at > ts_key(a))
            && match &self.upper {
                None => true,
                Some(Upper::Before(b)) => at < ts_key(b),
                Some(Upper::Until(u)) => at <= ts_key(u),
            }
    }
}

/// What a history call asks for: a channel or one thread, bounds and a page size. Everything in it is validated
/// when it is built, so a bad query is refused before anything is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryQuery {
    channel: String,
    thread: Option<Ts>,
    bounds: Bounds,
    limit: u16,
}

impl HistoryQuery {
    /// `thread`: the parent's `ts`, to read its replies instead of the channel. `limit` is 1 to `MAX_PAGE`.
    pub fn new(
        channel: &str,
        thread: Option<&str>,
        bounds: Bounds,
        limit: u16,
    ) -> Result<HistoryQuery, ApiError> {
        if !valid_id(channel) || !(1..=MAX_PAGE).contains(&limit) {
            return Err(ApiError::BadRequest);
        }
        Ok(HistoryQuery {
            channel: channel.into(),
            thread: thread
                .map(|t| Ts::parse(t).map_err(|_| ApiError::BadBound))
                .transpose()?,
            bounds,
            limit,
        })
    }

    /// The most records one page can hold.
    fn capacity(&self) -> usize {
        usize::from(self.limit) + usize::from(self.thread.is_some())
    }

    /// The Slack method and parameters of this query's read, with the cursor that continues it.
    fn request(&self, cursor: Option<&str>) -> (&'static str, Vec<(&'static str, String)>) {
        let mut params = vec![
            ("channel", self.channel.clone()),
            ("limit", self.limit.to_string()),
            ("include_all_metadata", "true".into()),
        ];
        let method = match &self.thread {
            Some(parent) => {
                params.push(("ts", parent.as_str().into()));
                "conversations.replies"
            }
            None => "conversations.history",
        };
        if let Some(after) = &self.bounds.after {
            params.push(("oldest", after.as_str().into()));
        }
        match &self.bounds.upper {
            None => {}
            Some(Upper::Before(t)) => {
                params.push(("latest", t.as_str().into()));
                params.push(("inclusive", "false".into()));
            }
            Some(Upper::Until(t)) => {
                params.push(("latest", t.as_str().into()));
                params.push(("inclusive", "true".into()));
            }
        }
        if let Some(cursor) = cursor {
            params.push(("cursor", cursor.into()));
        }
        (method, params)
    }

    /// The query as the values a continuation is bound to.
    fn fields(&self, workspace: &str) -> Vec<Value> {
        let (kind, upper) = match &self.bounds.upper {
            None => (Value::Null, Value::Null),
            Some(Upper::Before(t)) => ("before".into(), t.as_str().into()),
            Some(Upper::Until(t)) => ("until".into(), t.as_str().into()),
        };
        vec![
            workspace.into(),
            self.channel.as_str().into(),
            self.thread.as_ref().map(Ts::as_str).into(),
            self.bounds.after.as_ref().map(Ts::as_str).into(),
            kind,
            upper,
            self.limit.into(),
        ]
    }
}

/// Where a paged read goes on: Slack's cursor wrapped with the whole query it belongs to (workspace, channel,
/// thread, bounds, limit), capped at `MAX_CURSOR`. Another query refuses it as `Altered`. A caller may keep the text
/// (`as_str`, `parse`); it is opaque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Continuation(String);

impl Continuation {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A continuation kept as text. Anything over the cap or not made by this client is `Altered`.
    pub fn parse(text: String) -> Result<Continuation, ApiError> {
        if text.len() > MAX_CURSOR {
            return Err(ApiError::Altered);
        }
        Ok(Continuation(text))
    }

    fn wrap(workspace: &str, query: &HistoryQuery, slack: &str) -> Result<Continuation, ApiError> {
        let mut fields = query.fields(workspace);
        fields.push(slack.into());
        let text = Value::Array(fields).to_string();
        if text.len() > MAX_CURSOR {
            return Err(ApiError::TooLarge);
        }
        Ok(Continuation(text))
    }

    /// Slack's cursor, when this continuation was made for exactly this query.
    fn unwrap(&self, workspace: &str, query: &HistoryQuery) -> Result<String, ApiError> {
        let Ok(Value::Array(mut fields)) = serde_json::from_str::<Value>(&self.0) else {
            return Err(ApiError::Altered);
        };
        let slack = match fields.pop() {
            Some(Value::String(s)) if !s.is_empty() => s,
            _ => return Err(ApiError::Altered),
        };
        if fields != query.fields(workspace) {
            return Err(ApiError::Altered);
        }
        Ok(slack)
    }
}

/// One page: its records, and where to go on (`None`: this was the last page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub records: Vec<ChatRecord>,
    pub next: Option<Continuation>,
    /// Slack cut the history at the plan's horizon (`is_limited`); not an error.
    pub limited: bool,
}

/// What a scan asks for: delivery's one read of a channel or a thread, a fresh first page between two exclusive bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanQuery(HistoryQuery);

impl ScanQuery {
    /// `bounds` must hold `after` below `before` (an `until`, a missing bound or an empty window is `BadRequest`);
    /// `limit` is 1 to `MAX_SCAN`. Nothing is sent for a query that is refused here.
    pub fn new(
        channel: &str,
        thread: Option<&str>,
        bounds: Bounds,
        limit: u16,
    ) -> Result<ScanQuery, ApiError> {
        let (Some(after), Some(Upper::Before(before))) = (&bounds.after, &bounds.upper) else {
            return Err(ApiError::BadRequest);
        };
        if ts_key(after) >= ts_key(before) || !(1..=MAX_SCAN).contains(&limit) {
            return Err(ApiError::BadRequest);
        }
        // `HistoryQuery::new` bounds the limit at the display page; a scan's own bound is checked above.
        let q = HistoryQuery::new(channel, thread, bounds, 1)?;
        Ok(ScanQuery(HistoryQuery { limit, ..q }))
    }
}

/// A scan's page. `oldest` and `newest` are the raw extremes of the page's `ts` whatever order Slack listed them (a
/// replies page's parent apart); `threads` is every message's `(ts, latest_reply)` as Slack sent it, a deleted
/// parent's and one above the read's `latest` included; `records` are the representable messages inside the bounds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanPage {
    pub records: Vec<ChatRecord>,
    pub threads: Vec<(Ts, Ts)>,
    pub oldest: Option<Ts>,
    pub newest: Option<Ts>,
    pub has_more: bool,
    /// Slack's `is_limited`, reported and never an error.
    pub limited: bool,
}

/// Whose post this is. The kind is carried in a row's metadata and username suffix, as `normalize` reads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostKind {
    /// A person at the window.
    Person,
    /// A row.
    Row,
}

impl PostKind {
    fn word(self) -> &'static str {
        match self {
            PostKind::Person => "person",
            PostKind::Row => "row",
        }
    }
}

/// How a post ended. It was submitted at most once, whatever the outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostOutcome {
    /// Slack acknowledged it with this `ts`.
    Posted(Ts),
    /// Definitely not posted: Slack's rejection (its code), or the request never left (`busy`, `invalid`,
    /// `destination`).
    Refused(String),
    /// Slack was rate limited; nothing was posted and nothing is resent. The caller keeps its draft.
    Limited {
        retry_after: Duration,
        assumed: bool,
    },
    /// No usable acknowledgement: the post may or may not exist. Never resent; the caller reads history.
    Unknown,
}

/// One of the `MAX_TRANSFERS` transfer permits, from `Api::transfer`: held by whoever reads a file for a post, so no
/// file is read into memory before its permit, and by `Api::post_file` across all three steps; freed when dropped.
pub struct Transfer(#[allow(dead_code)] OwnedSemaphorePermit);

/// The `MAX_TRANSFERS` permits: clones share them, so every `Api` given one clone draws on the same two. The daemon makes
/// one per chat and gives it to every workspace's service, so it holds at most two files whatever the workspaces.
#[derive(Clone)]
pub struct Transfers(Arc<Semaphore>);

impl Transfers {
    pub fn new() -> Transfers {
        Transfers(Arc::new(Semaphore::new(MAX_TRANSFERS)))
    }
}

impl Default for Transfers {
    fn default() -> Transfers {
        Transfers::new()
    }
}

/// How a file post ended. Only the share (the third step) can post a file, and it was submitted at most once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileOutcome {
    /// Shared: the file's id, and the share's `ts` when Slack's answer names it.
    Posted { file: String, ts: Option<Ts> },
    /// Definitely not shared: Slack's rejection of the share (its code), or any failure before the share was sent
    /// (the code of the step that failed, `closed` for a client closed between the upload and the share).
    Refused(String),
    /// Slack was rate limited at the first step or the share; nothing was shared and nothing is resent.
    Limited {
        retry_after: Duration,
        assumed: bool,
    },
    /// The share was sent and not acknowledged: the file may or may not be in the channel. Never resent.
    Unknown,
}

/// Which rendition of a file a fetch reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileVariant {
    /// Slack's small rendition (at most `MAX_THUMB` bytes), or a small original when Slack made none.
    Thumb,
    /// The file as posted (at most `MAX_CHAT_FILE` bytes).
    Original,
}

/// What a fetch of a picture came to short of an error: Slack's answer was a picture, or the call knows it is not one
/// the chat carries. Nothing is shared or changed either way.
#[derive(Clone, PartialEq, Eq)]
pub enum FileFetch {
    /// The picture's bytes, and the `image/*` type Slack served them as.
    Image { mime: String, bytes: Vec<u8> },
    /// The file's type is not a picture the chat shows, or Slack answered something that is not an image (its sign-in
    /// page).
    NotAnImage,
    /// The file or its rendition is over the bound of its variant, by Slack's word or by the bytes counted as they came.
    TooLarge,
    /// A thumbnail was asked for, Slack names none and the original is over `MAX_THUMB`. Not final: Slack can make a
    /// thumbnail after the upload, so a later ask may find one.
    NoThumbnail,
}

impl fmt::Debug for FileFetch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileFetch::Image { mime, bytes } => write!(f, "Image {{ mime: {mime:?}, bytes: {} }}", bytes.len()),
            FileFetch::NotAnImage => f.write_str("NotAnImage"),
            FileFetch::TooLarge => f.write_str("TooLarge"),
            FileFetch::NoThumbnail => f.write_str("NoThumbnail"),
        }
    }
}

#[derive(Clone, Copy)]
struct Deadlines {
    read: Duration,
    post: Duration,
    names: Duration,
    transfer: Duration,
    /// How long a member list serves posts (`MEMBERS_FRESH`).
    fresh: Duration,
}

#[derive(Default)]
struct Names {
    by_id: HashMap<String, String>,
    /// Insertion order, oldest first, so a full cache drops the oldest.
    order: VecDeque<String>,
}

/// The Slack Web API for one workspace.
pub struct Api {
    http: Http,
    token: Secret,
    /// The user token a person's post goes out under. Forgotten when Slack refuses it; never replaced by `token`.
    person: Mutex<Option<Secret>>,
    workspace: String,
    base: String,
    deadlines: Deadlines,
    own: Mutex<Option<Identity>>,
    /// Reads are not eligible before this instant (the last rate limit's retry header).
    eligible: Mutex<Option<Instant>>,
    names: Mutex<Names>,
    /// The workspace's people as the last whole `users.list` walk read them, and when (guarantee 35).
    members: tokio::sync::Mutex<Option<(Instant, Arc<Roster>)>>,
    closed: AtomicBool,
    /// The permits it draws on, each held across every step of one transfer: its own unless it was given shared ones.
    transfers: Transfers,
    /// The workspace's name as the last `auth.test` gave it (`team`, cleaned), for words only.
    name: Mutex<Option<String>>,
    /// The service's end: a refusal of the bot's token ends it, and an ended service's client is closed.
    end: Arc<End>,
}

/// A bot token's workspace and bot, as one `auth.test` names them before any workspace is configured, and the workspace's
/// name when Slack gives a usable one (`team`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identified {
    pub workspace: String,
    pub own: Identity,
    pub name: Option<String>,
}

/// Who a token (the bot's, or the user token's) is, with no workspace expected: one `auth.test` under the read rules (guarantee 28). The setup asks
/// it before anything is saved; the service's `Api::auth_test` keeps its workspace check.
pub async fn identify(http: &Http, bot: Secret) -> Result<Identified, ApiError> {
    identify_at(http, bot, BASE.into()).await
}

/// Slack's error codes that name the token itself as bad, as against the request or Slack's own state (a rate limit, an
/// internal error, an outage): only these make a setup check a refusal of the token.
pub const TOKEN_REFUSALS: [&str; 6] =
    ["not_authed", "invalid_auth", "token_revoked", "token_expired", "account_inactive", "not_allowed_token_type"];

/// Whether a Slack code names the token or its scope as bad (`TOKEN_REFUSALS` or `missing_scope`): the codes a person's
/// token is forgotten for, and the daemon's reply `no_user_token` is made of.
pub fn refuses_the_token(code: &str) -> bool {
    TOKEN_REFUSALS.contains(&code) || code == "missing_scope"
}

/// `identify` against the fixture's base (`FakeSlack::url("slack.com", "/api/")`), for another crate's tests. Exists
/// under `cfg(test)` or the `test-support` feature alone, like `Service::start_at`.
#[cfg(any(test, feature = "test-support"))]
pub async fn identify_for_test(http: &Http, bot: Secret, base: String) -> Result<Identified, ApiError> {
    identify_at(http, bot, base).await
}

/// `identify` against a base: the product's from `identify`, the fixture's from a test.
pub(crate) async fn identify_at(http: &Http, bot: Secret, base: String) -> Result<Identified, ApiError> {
    let api = Api::build(http.clone(), bot, String::new(), base);
    let v = api.read("auth.test", &[], MAX_BODY).await?;
    Ok(Identified {
        workspace: workspace_of(&v)?.into(),
        own: identity_of(&v)?,
        name: name_of(&v),
    })
}

/// The Slack app of a bot token no workspace is configured with yet: one `bots.info` for `bot` (the bot's id as `identify`
/// gave it) under the token, by the read rules, answered as [`Api::bot_app`] answers (`BadRequest`, unsent, for a `bot`
/// that is no id). The setup asks it on every connect that does not say to replace the app connected.
pub async fn bot_app_of(http: &Http, token: Secret, bot: &str) -> Result<BotApp, ApiError> {
    bot_app_at(http, token, bot, BASE.into()).await
}

/// `bot_app_of` against the fixture's base, for another crate's tests, under the same features as `identify_for_test`.
#[cfg(any(test, feature = "test-support"))]
pub async fn bot_app_of_for_test(http: &Http, token: Secret, bot: &str, base: String) -> Result<BotApp, ApiError> {
    bot_app_at(http, token, bot, base).await
}

pub(crate) async fn bot_app_at(http: &Http, token: Secret, bot: &str, base: String) -> Result<BotApp, ApiError> {
    if !valid_id(bot) {
        return Err(ApiError::BadRequest);
    }
    let api = Api::build(http.clone(), token.clone(), String::new(), base);
    bot_app_in(&api.read_as(token, "bots.info", &[("bot", bot.into())], MAX_BODY).await?)
}

/// A `bots.info` answer's app: `bot.app_id`, required and an id (`Malformed` otherwise), and `bot.name`, read as a
/// workspace's name is.
fn bot_app_in(v: &Value) -> Result<BotApp, ApiError> {
    let app = v.pointer("/bot/app_id").and_then(Value::as_str).filter(|a| valid_id(a)).ok_or(ApiError::Malformed)?;
    Ok(BotApp { id: app.into(), name: v.get("bot").and_then(|b| name_at(b, "name")) })
}

impl Api {
    /// `workspace` is the Slack team id the token must belong to.
    pub fn new(http: Http, token: Secret, workspace: &str) -> Result<Api, ApiError> {
        if !valid_id(workspace) {
            return Err(ApiError::BadRequest);
        }
        Ok(Api::build(http, token, workspace.into(), BASE.into()))
    }

    fn build(http: Http, token: Secret, workspace: String, base: String) -> Api {
        Api {
            http,
            token,
            person: Mutex::new(None),
            workspace,
            base,
            deadlines: Deadlines {
                read: READ_DEADLINE,
                post: POST_DEADLINE,
                names: NAMES_DEADLINE,
                transfer: TRANSFER_DEADLINE,
                fresh: MEMBERS_FRESH,
            },
            own: Mutex::new(None),
            eligible: Mutex::new(None),
            names: Mutex::new(Names::default()),
            members: tokio::sync::Mutex::new(None),
            closed: AtomicBool::new(false),
            transfers: Transfers::new(),
            name: Mutex::new(None),
            end: End::detached(),
        }
    }

    /// This client drawing on `transfers`, shared with every other client given them.
    pub fn with_transfers(mut self, transfers: Transfers) -> Api {
        self.transfers = transfers;
        self
    }

    /// The Slack team id this client serves.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// The workspace's name as the last `auth.test` gave it; `None` before one, or when Slack gave none usable.
    pub fn workspace_name(&self) -> Option<String> {
        self.name.lock().unwrap().clone()
    }

    /// This client under its service's end.
    pub(crate) fn with_end(mut self, end: Arc<End>) -> Api {
        self.end = end;
        self
    }

    /// Closed by a stop, or its service ended.
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) || self.end.code().is_some()
    }

    /// This client with the person's user token (`None`: a person's post is refused unsent).
    pub fn with_person(mut self, token: Option<Secret>) -> Api {
        self.person = Mutex::new(token);
        self
    }

    /// Whether a person's token is held now, so a caller refuses a person's post before it reads anything for it.
    pub fn holds_person_token(&self) -> bool {
        self.person.lock().unwrap().is_some()
    }

    /// The token a post of `kind` goes out under: the bot's for a row, the person's for a person, and never the bot's in
    /// its place.
    fn poster(&self, kind: PostKind) -> Result<Secret, ApiError> {
        match kind {
            PostKind::Row => Ok(self.token.clone()),
            PostKind::Person => self.person.lock().unwrap().clone().ok_or(ApiError::NoPersonToken),
        }
    }

    /// Slack refused a call made under `kind`'s token with `code`. A person's token is forgotten when the code names
    /// the token or its scope as bad (the refusal itself is still the caller's answer; a restart reads the file again);
    /// the bot's, a row's, ends the service when the code names the token (`TOKEN_REFUSALS`; `missing_scope` names a
    /// scope and ends nothing).
    fn refused(&self, kind: PostKind, code: &str) {
        match kind {
            PostKind::Person if refuses_the_token(code) => *self.person.lock().unwrap() = None,
            PostKind::Row if TOKEN_REFUSALS.contains(&code) => self.end.end(BOT_TOKEN_REFUSED),
            _ => {}
        }
    }

    /// Refuse every later call, on every handle. A call already sent runs to its own deadline.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// The fixture's logical base URL; the product has no endpoint override.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn with_base(mut self, base: String) -> Api {
        self.base = base;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_deadlines(mut self, read: Duration, post: Duration, names: Duration) -> Api {
        self.deadlines = Deadlines { read, post, names, ..self.deadlines };
        self
    }

    #[cfg(test)]
    pub(crate) fn with_members_fresh(mut self, fresh: Duration) -> Api {
        self.deadlines.fresh = fresh;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_transfer_deadline(mut self, transfer: Duration) -> Api {
        self.deadlines.transfer = transfer;
        self
    }

    #[cfg(test)]
    pub(crate) fn names_cached(&self) -> usize {
        self.names.lock().unwrap().by_id.len()
    }

    #[cfg(test)]
    pub(crate) fn is_cached(&self, user: &str) -> bool {
        self.names.lock().unwrap().by_id.contains_key(user)
    }

    /// An `Api` that already holds the bot identity, as if `auth_test` had answered.
    #[cfg(test)]
    pub(crate) fn with_identity(self, own: Identity) -> Api {
        *self.own.lock().unwrap() = Some(own);
        self
    }

    /// The person's user token checked once, at the service's start: one `auth.test` under it. A `team_id` other than this
    /// client's workspace, or a code of `TOKEN_REFUSALS`, drops the token (a person's post is then refused unsent,
    /// `NoPersonToken`) and answers why, a fixed code (`user_token_other_workspace`, `user_token_refused`); Slack's own
    /// state or the transport keeps it, and so does a client that holds none (`None`).
    pub(crate) async fn check_person(&self) -> Option<&'static str> {
        let token = self.person.lock().unwrap().clone()?;
        let dropped = match self.read_as(token, "auth.test", &[], MAX_BODY).await {
            Ok(v) if workspace_of(&v).is_ok_and(|w| w != self.workspace) => "user_token_other_workspace",
            Err(ApiError::Slack(code)) if TOKEN_REFUSALS.contains(&code.as_str()) => "user_token_refused",
            _ => return None,
        };
        *self.person.lock().unwrap() = None;
        Some(dropped)
    }

    /// The bot's app (`bots.info`'s `bot.app_id` for the identity's `bot_id`, and its `bot.name`; `NoIdentity`, unsent, for
    /// a bot with none). Its failure, any code, only leaves the app unknown and ends nothing: the check it serves fails open.
    pub async fn bot_app(&self) -> Result<BotApp, ApiError> {
        let bot = self.identity()?.bot_id.ok_or(ApiError::NoIdentity)?;
        bot_app_in(&self.read_as(self.token.clone(), "bots.info", &[("bot", bot)], MAX_BODY).await?)
    }

    /// The bot identity, which history needs. The token must belong to the configured workspace.
    pub async fn auth_test(&self) -> Result<Identity, ApiError> {
        let v = self.read("auth.test", &[], MAX_BODY).await?;
        if workspace_of(&v)? != self.workspace {
            return Err(ApiError::WrongWorkspace);
        }
        let identity = identity_of(&v)?;
        *self.own.lock().unwrap() = Some(identity.clone());
        *self.name.lock().unwrap() = name_of(&v);
        Ok(identity)
    }

    /// One page of a channel's history (newest first, paging backward) or of a thread's replies (paging forward).
    /// `cont` continues a page this client returned for this same query. The page is whole or an error.
    pub async fn history(
        &self,
        query: &HistoryQuery,
        cont: Option<&Continuation>,
    ) -> Result<Page, ApiError> {
        // Slack's replies cursor under a lower bound alone drops a reply at every page boundary (guarantee 11).
        let lower_alone = query.bounds.after.is_some() && query.bounds.upper.is_none();
        if cont.is_some() && query.thread.is_some() && lower_alone {
            return Err(ApiError::BadRequest);
        }
        let own = self.identity()?;
        let sent = cont.map(|c| c.unwrap(&self.workspace, query)).transpose()?;
        let (method, params) = query.request(sent.as_deref());
        let v = self.read(method, &params, MAX_BODY).await?;
        let (messages, has_more, limited) = page_parts(&v, query.capacity())?;
        let next = if has_more {
            let slack = v
                .pointer("/response_metadata/next_cursor")
                .and_then(Value::as_str)
                .unwrap_or("");
            if slack.is_empty() {
                return Err(ApiError::Missing);
            }
            if sent.as_deref() == Some(slack) {
                return Err(ApiError::Repeated);
            }
            Some(Continuation::wrap(&self.workspace, query, slack)?)
        } else {
            None
        };
        let ctx = Context {
            workspace: &self.workspace,
            own_bot_id: own.bot_id.as_deref(),
            own_user_id: Some(&own.user_id),
            limited,
        };
        let mut records = Vec::new();
        for raw in messages {
            match normalize(&ctx, &query.channel, raw) {
                Normalized::New(rec) => {
                    // Slack's inclusivity at the edges is not relied on; the thread's parent is exempt.
                    if query.thread.as_ref() == Some(&rec.ts) || query.bounds.admits(&rec.ts) {
                        records.push(rec);
                    }
                }
                Normalized::Unsupported => {}
                _ => return Err(ApiError::Malformed),
            }
        }
        Ok(Page {
            records,
            next,
            limited,
        })
    }

    /// Delivery's one read of a channel or a thread: a fresh first page with both bounds (`inclusive=false`), up to
    /// 999 messages, never a cursor, under its own response bound. The caller reads on by lowering `before`.
    pub async fn scan(&self, query: &ScanQuery) -> Result<ScanPage, ApiError> {
        let (own, query) = (self.identity()?, &query.0);
        let (method, params) = query.request(None);
        let v = self.read(method, &params, MAX_SCAN_BODY).await?;
        let (messages, has_more, limited) = page_parts(&v, query.capacity())?;
        let ctx = Context {
            workspace: &self.workspace,
            own_bot_id: own.bot_id.as_deref(),
            own_user_id: Some(&own.user_id),
            limited,
        };
        let mut page = ScanPage {
            has_more,
            limited,
            ..ScanPage::default()
        };
        let ts_of = |raw: &Value, key: &str| {
            let ts = raw.get(key).and_then(Value::as_str);
            ts.and_then(|t| Ts::parse(t).ok())
                .ok_or(ApiError::Malformed)
        };
        for raw in messages {
            let ts = ts_of(raw, "ts")?;
            if raw.get("latest_reply").is_some_and(|r| !r.is_null()) {
                page.threads.push((ts.clone(), ts_of(raw, "latest_reply")?));
            }
            if query.thread.as_ref() != Some(&ts) {
                let key = ts_key(&ts);
                if page.oldest.as_ref().is_none_or(|o| key < ts_key(o)) {
                    page.oldest = Some(ts.clone());
                }
                if page.newest.as_ref().is_none_or(|n| key > ts_key(n)) {
                    page.newest = Some(ts);
                }
            }
            match normalize(&ctx, &query.channel, raw) {
                Normalized::New(rec) if query.bounds.admits(&rec.ts) => page.records.push(rec),
                Normalized::New(_) | Normalized::Unsupported => {}
                _ => return Err(ApiError::Malformed),
            }
        }
        // More behind a page that gave nothing to walk back from can never end.
        if has_more && page.oldest.is_none() {
            return Err(ApiError::Malformed);
        }
        Ok(page)
    }

    /// The bot identity as `auth_test` last answered it.
    fn identity(&self) -> Result<Identity, ApiError> {
        self.own.lock().unwrap().clone().ok_or(ApiError::NoIdentity)
    }

    /// `post_linked`'s outcome alone.
    pub async fn post(
        &self,
        channel: &str,
        thread: Option<&str>,
        kind: PostKind,
        name: &str,
        text: &str,
    ) -> Result<PostOutcome, ApiError> {
        self.post_linked(channel, thread, kind, name, text).await.map(|(outcome, _)| outcome)
    }

    /// Post once: a row's under a custom author, a person's under their own user token and with nothing of the app
    /// (`Err(NoPersonToken)` unsent when none is held; never the bot's token instead). `thread`: a parent's `ts`, to reply. Input the post cannot carry is refused before anything is sent (`Err`); from
    /// there the result is a `PostOutcome`, and no path resends. The text's `@` names are linked (`linked`), and the
    /// names left as text are returned with the outcome.
    pub async fn post_linked(
        &self,
        channel: &str,
        thread: Option<&str>,
        kind: PostKind,
        name: &str,
        text: &str,
    ) -> Result<(PostOutcome, Vec<ChatUnlinked>), ApiError> {
        if self.is_closed() {
            return Err(ApiError::Closed);
        }
        let thread = thread
            .map(|t| Ts::parse(t).map_err(|_| ApiError::BadBound))
            .transpose()?;
        if !valid_id(channel) || (kind == PostKind::Row && !author_ok(name)) {
            return Err(ApiError::BadRequest);
        }
        if text.is_empty() || !post_text_fits(text) {
            return Err(ApiError::BadRequest);
        }
        let token = self.poster(kind)?;
        let linked = self.linked(text, kind, MAX_TEXT).await?;
        let mut body = json!({"channel": channel, "text": linked.text});
        if kind == PostKind::Row {
            body["username"] = format!("{name} ({} via Ship of Tools)", kind.word()).into();
            body["metadata"] = json!({
                "event_type": "sot_chat_post",
                "event_payload": {"kind": kind.word(), "name": name},
            });
        }
        if let Some(parent) = thread {
            body["thread_ts"] = parent.as_str().into();
        }
        let request = Request::post(
            self.url("chat.postMessage", &[])?,
            self.deadlines.post,
            JSON,
            body.to_string().into_bytes(),
        )
        .bearer(token);
        let outcome = post_outcome(self.http.send(request).await);
        if let PostOutcome::Refused(code) = &outcome {
            self.refused(kind, code);
        }
        Ok((outcome, linked.unlinked))
    }

    /// One transfer permit, taken at once: `Err(Http(Busy))` while `MAX_TRANSFERS` are held, `Err(Closed)` for a closed
    /// client. Owned, so a caller that drops it (or a future holding it) frees it.
    pub fn transfer(&self) -> Result<Transfer, ApiError> {
        if self.is_closed() {
            return Err(ApiError::Closed);
        }
        self.transfers.0.clone().try_acquire_owned().map(Transfer).map_err(|_| ApiError::Http(HttpError::Busy))
    }

    /// Share a file once, as `kind` named `name`: Slack's three steps under the caller's transfer permit. Step 1 is a read
    /// (`files.getUploadURLExternal`, under the read rules); step 2 posts the bytes to the URL it answered, with no
    /// authorization; step 3 (`files.completeUploadExternal`) is the share, the only mutation, read as a post's answer
    /// is. Its `initial_comment` is the label `<name> (<kind> via Ship of Tools)`, then `: ` and the comment when there
    /// is one, made Slack's by `slack_text`. A person's share makes steps 1 and 3 under their own user token, never the
    /// bot's (`Err(NoPersonToken)` unsent when none is held), its `initial_comment` the comment alone and absent without
    /// one, and `name` neither checked nor sent; a refusal of either step that names the token forgets it, as a post's
    /// does. Input the post cannot carry is `Err` unsent; from there the result is a `FileOutcome`, a failure before the
    /// share is never `Unknown`, and no path resends.
    #[allow(clippy::too_many_arguments)]
    pub async fn post_file(
        &self,
        transfer: Transfer,
        channel: &str,
        thread: Option<&str>,
        kind: PostKind,
        name: &str,
        file_name: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
    ) -> Result<FileOutcome, ApiError> {
        let shared = self.post_file_linked(transfer, channel, thread, kind, name, file_name, bytes, comment).await;
        shared.map(|(outcome, _)| outcome)
    }

    /// `post_file`, with the comment's `@` names linked as a post's are (the row's label never is) and the names left as
    /// text returned with the outcome.
    #[allow(clippy::too_many_arguments)]
    pub async fn post_file_linked(
        &self,
        _transfer: Transfer,
        channel: &str,
        thread: Option<&str>,
        kind: PostKind,
        name: &str,
        file_name: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
    ) -> Result<(FileOutcome, Vec<ChatUnlinked>), ApiError> {
        if self.is_closed() {
            return Err(ApiError::Closed);
        }
        let thread = thread
            .map(|t| Ts::parse(t).map_err(|_| ApiError::BadBound))
            .transpose()?;
        let file_name_ok = !file_name.is_empty()
            && file_name.len() <= MAX_FILENAME
            && !file_name.contains(|c: char| c.is_control() || c == '/' || c == '\\');
        let bytes_ok = !bytes.is_empty() && bytes.len() <= MAX_CHAT_FILE;
        if !valid_id(channel) || (kind == PostKind::Row && !author_ok(name)) || !file_name_ok || !bytes_ok || comment == Some("") {
            return Err(ApiError::BadRequest);
        }
        // A row's comment rides after its label, which is escaped and never linked.
        let label = (kind == PostKind::Row).then(|| format!("{name} ({} via Ship of Tools)", kind.word()));
        let typed = [label.as_deref(), comment].into_iter().flatten().collect::<Vec<_>>().join(": ");
        if (label.is_some() || comment.is_some()) && !post_text_fits(&typed) {
            return Err(ApiError::BadRequest);
        }
        let share_url = self.url("files.completeUploadExternal", &[])?;
        let token = self.poster(kind)?;
        let (comment, unlinked) = match (label, comment) {
            (label, Some(c)) => {
                let prefix = label.map(|l| escape_text(&format!("{l}: "))).unwrap_or_default();
                let linked = self.linked(c, kind, MAX_TEXT - prefix.len()).await?;
                (Some(prefix + &linked.text), linked.unlinked)
            }
            (label, None) => (label.map(|l| escape_text(&l)), Vec::new()),
        };
        // The share's three steps: any return here is the outcome, with the names left as text.
        let outcome = async {
            let params = [("filename", file_name.to_string()), ("length", bytes.len().to_string())];
            let v = match self.read_as(token.clone(), "files.getUploadURLExternal", &params, MAX_BODY).await {
                Ok(v) => v,
                Err(ApiError::Limited { retry_after, assumed }) => return FileOutcome::Limited { retry_after, assumed },
                Err(e) => {
                    let code = refusal(&e);
                    self.refused(kind, &code);
                    return FileOutcome::Refused(code);
                }
            };
            let upload_url = v.get("upload_url").and_then(Value::as_str).filter(|u| u.len() <= MAX_URL);
            let (Some(file), Some(upload_url)) = (answered_id(&v, "file_id"), upload_url) else {
                return FileOutcome::Refused(ApiError::Malformed.code().into());
            };
            // The upload URL carries its own grant: no bearer, and `Http` refuses a host that is not Slack's before a byte.
            let upload = Request::post(upload_url, self.deadlines.transfer, "application/octet-stream", bytes);
            match self.http.send(upload).await {
                Ok(r) if (200..=299).contains(&r.status()) => {}
                Ok(r) => return FileOutcome::Refused(format!("http_{}", r.status())),
                Err(e) => return FileOutcome::Refused(e.code().into()),
            }
            if self.is_closed() {
                return FileOutcome::Refused(ApiError::Closed.code().into());
            }
            let mut body = json!({
                "files": [{"id": file, "title": file_name}],
                "channel_id": channel,
            });
            if let Some(comment) = comment {
                body["initial_comment"] = comment.into();
            }
            if let Some(parent) = thread {
                body["thread_ts"] = parent.as_str().into();
            }
            let share = Request::post(share_url, self.deadlines.post, JSON, body.to_string().into_bytes()).bearer(token);
            let outcome = share_outcome(self.http.send(share).await, channel);
            if let FileOutcome::Refused(code) = &outcome {
                self.refused(kind, code);
            }
            outcome
        }
        .await;
        Ok((outcome, unlinked))
    }

    /// Read one picture of a channel's, under one transfer permit (guarantee 33): `files.info` for the file, which must be
    /// shared in `channel` and a PNG, JPEG, GIF or WebP, then one GET of the rendition, with the bearer, bounded by the
    /// variant (`MAX_THUMB` or `MAX_CHAT_FILE`) while it streams. A thumbnail is Slack's `thumb_360`, `thumb_480` or
    /// `thumb_160`, else a small original (`NoThumbnail` when it is not small). Input it cannot carry is `Err` unsent, and a
    /// file that is not the channel's is `Err` with nothing downloaded.
    pub async fn fetch_file(&self, channel: &str, file: &str, variant: FileVariant) -> Result<FileFetch, ApiError> {
        if self.is_closed() {
            return Err(ApiError::Closed);
        }
        if !valid_id(channel) || !valid_id(file) {
            return Err(ApiError::BadRequest);
        }
        let _permit = self.transfer()?;
        let v = self.read("files.info", &[("file", file.into())], MAX_BODY).await?;
        let info = v.get("file").filter(|f| answered_id(f, "id") == Some(file)).ok_or(ApiError::Malformed)?;
        if !shared_in(info, channel) {
            return Err(ApiError::BadRequest);
        }
        if !info.get("mimetype").and_then(Value::as_str).is_some_and(|m| PICTURE_MIMES.contains(&m)) {
            return Ok(FileFetch::NotAnImage);
        }
        let url = |key: &str| info.get(key).and_then(Value::as_str).filter(|u| !u.is_empty() && u.len() <= MAX_URL);
        let (bound, deadline) = match variant {
            FileVariant::Thumb => (MAX_THUMB, self.deadlines.read),
            FileVariant::Original => (MAX_CHAT_FILE, self.deadlines.transfer),
        };
        let thumb = (variant == FileVariant::Thumb).then(|| ["thumb_360", "thumb_480", "thumb_160"].into_iter().find_map(url));
        let url = match thumb.flatten() {
            Some(thumb) => thumb,
            // The original: its size, by Slack's word, is checked before any byte is asked for.
            None => {
                let original = url("url_private_download").or_else(|| url("url_private")).ok_or(ApiError::Malformed)?;
                match info.get("size").and_then(Value::as_u64) {
                    Some(size) if size <= bound as u64 => original,
                    // A thumbnail Slack has not made (it can follow the upload) is not an original too large to show.
                    Some(_) if variant == FileVariant::Thumb => return Ok(FileFetch::NoThumbnail),
                    Some(_) => return Ok(FileFetch::TooLarge),
                    None => return Err(ApiError::Malformed),
                }
            }
        };
        // A service retired while `files.info` was answered sends nothing more, as a share's last step does not.
        if self.is_closed() {
            return Err(ApiError::Closed);
        }
        let request = Request::get(url, deadline).max_body(bound).bearer(self.token.clone());
        let response = match self.http.send(request).await {
            Ok(response) => response,
            Err(HttpError::TooLarge) => return Ok(FileFetch::TooLarge),
            Err(e) => return Err(ApiError::Http(e)),
        };
        match response.status() {
            429 => return Err(self.rate_limited(&response)),
            200..=299 => {}
            other => return Err(ApiError::Status(other)),
        }
        // Slack answers a sign-in page, as HTML, to a download it will not serve.
        let Some(mime) = image_type(&response) else {
            return Ok(FileFetch::NotAnImage);
        };
        if response.body().is_empty() {
            return Err(ApiError::Malformed);
        }
        Ok(FileFetch::Image { mime, bytes: response.body().to_vec() })
    }

    /// The channels the bot can see: every public channel and the private ones it is in, as many pages as `MAX_LISTED`
    /// allows, each page one read. The list is whole or an error; `more` says Slack had another page when it stopped.
    pub async fn channels(&self) -> Result<ChannelList, ApiError> {
        let params = [("types", "public_channel,private_channel".to_string()), ("exclude_archived", "true".to_string())];
        let (channels, more) = self.list("conversations.list", &params, "channels", (LIST_PAGE, MAX_LIST_PAGES, MAX_BODY), listed).await?;
        Ok(ChannelList { channels, more })
    }

    /// A list method read whole: pages of `page` items under `params`, at most `pages` of them each bounded by `body`,
    /// each item of `key` kept through `keep`. `more` says Slack had another page when it stopped. A page that is not
    /// one, a cursor past `MAX_CURSOR` (`TooLarge`) or the one just sent (`Repeated`) fails the whole list.
    async fn list<T>(
        &self,
        method: &str,
        params: &[(&'static str, String)],
        key: &str,
        (page, pages, body): (usize, usize, usize),
        keep: impl Fn(&Value) -> Option<T>,
    ) -> Result<(Vec<T>, bool), ApiError> {
        let mut items = Vec::new();
        let mut sent: Option<String> = None;
        for _ in 0..pages {
            let mut params = params.to_vec();
            params.push(("limit", page.to_string()));
            params.extend(sent.clone().map(|c| ("cursor", c)));
            let v = self.read(method, &params, body).await?;
            let listed = v.get(key).and_then(Value::as_array).filter(|c| c.len() <= page).ok_or(ApiError::Malformed)?;
            items.extend(listed.iter().filter_map(&keep));
            let next = v.pointer("/response_metadata/next_cursor").and_then(Value::as_str).unwrap_or("");
            if next.is_empty() {
                return Ok((items, false));
            }
            if next.len() > MAX_CURSOR {
                return Err(ApiError::TooLarge);
            }
            if sent.as_deref() == Some(next) {
                return Err(ApiError::Repeated);
            }
            sent = Some(next.into());
        }
        Ok((items, true))
    }

    /// `text` as a post sends it (guarantee 35): escaped, with its `@` names linked within `room` bytes. The member list
    /// is read only when a name needs it, and a member linked is remembered by the name cache, so the post read back shows
    /// the name at once. The caller has checked the text fits; a client closed meanwhile sends nothing.
    async fn linked(&self, text: &str, kind: PostKind, room: usize) -> Result<Linked, ApiError> {
        let mut linked = link(text, kind, Err(ChatUnlinkedWhy::MembersUnread), room);
        if linked.wanted {
            linked = match self.roster().await.as_deref() {
                Ok(Roster::Read(members)) => link(text, kind, Ok(members), room),
                Ok(Roster::TooMany) => link(text, kind, Err(ChatUnlinkedWhy::TooManyMembers), room),
                Err(_) => linked,
            };
        }
        linked.linked.iter().for_each(|(id, shown)| self.remember(id, shown));
        if self.is_closed() {
            return Err(ApiError::Closed);
        }
        Ok(linked)
    }

    /// The workspace's people: the last walk's while `MEMBERS_FRESH`, else one `users.list` walk under the bot's token,
    /// all within `NAMES_DEADLINE` (another post's walk waited for included). A walk that fails or runs out of time is
    /// `Err` and is not remembered; one that finds more than `MAX_MEMBERS` is remembered as `TooMany`.
    async fn roster(&self) -> Result<Arc<Roster>, ()> {
        let walk = async {
            let mut held = self.members.lock().await;
            if let Some((_, roster)) = held.as_ref().filter(|(at, _)| at.elapsed() < self.deadlines.fresh) {
                return Ok(roster.clone());
            }
            let page = (MEMBER_PAGE, MAX_MEMBER_PAGES, MEMBERS_BODY);
            let (members, more) = self.list("users.list", &[], "members", page, Member::of).await.map_err(|_| ())?;
            let roster = Arc::new(if more { Roster::TooMany } else { Roster::Read(Index::new(members)) });
            *held = Some((Instant::now(), roster.clone()));
            Ok(roster)
        };
        tokio::time::timeout(self.deadlines.names, walk).await.unwrap_or(Err(()))
    }

    /// One channel's facts, from one `conversations.info`. An id that is no channel's is refused unsent.
    pub async fn channel(&self, id: &str) -> Result<Channel, ApiError> {
        if !valid_id(id) || !is_channel_id(id) {
            return Err(ApiError::BadRequest);
        }
        let v = self.read("conversations.info", &[("channel", id.into())], MAX_BODY).await?;
        let raw = v.get("channel").ok_or(ApiError::Malformed)?;
        channel_facts(raw, id.into()).ok_or(ApiError::Malformed)
    }

    /// Join a public channel once. Input that is no channel's id is `Err` unsent; from there the result is a
    /// `JoinOutcome`, and no path resends.
    pub async fn join(&self, channel: &str) -> Result<JoinOutcome, ApiError> {
        if self.is_closed() {
            return Err(ApiError::Closed);
        }
        if !valid_id(channel) || !is_channel_id(channel) {
            return Err(ApiError::BadRequest);
        }
        let request = Request::post(
            self.url("conversations.join", &[])?,
            self.deadlines.post,
            JSON,
            json!({"channel": channel}).to_string().into_bytes(),
        )
        .bearer(self.token.clone());
        Ok(match acknowledgement(self.http.send(request).await) {
            Ack::Done(_) => JoinOutcome::Joined,
            Ack::Refused(code) if code == "already_in_channel" => JoinOutcome::Joined,
            Ack::Refused(code) => {
                self.refused(PostKind::Row, &code);
                JoinOutcome::Refused(code)
            }
            Ack::Limited { .. } | Ack::Unknown => JoinOutcome::Unknown,
        })
    }

    /// A member's display name: `display_name`, then `real_name`, then `name`, from the cache or `users.info`. Any
    /// failure gives the id, and a failure is not remembered.
    pub async fn author_name(&self, user: &str) -> String {
        self.name_of(user, false).await.unwrap_or_else(|_| user.to_string())
    }

    /// The name from the cache or `users.info` (`conversations.info` for a channel's id, `channel_name`'s rule for a
    /// `mapped` one), remembered on a success.
    async fn name_of(&self, user: &str, mapped: bool) -> Result<String, ApiError> {
        if let Some(name) = self.names.lock().unwrap().by_id.get(user) {
            return Ok(name.clone());
        }
        let name = self.lookup(user, mapped).await?;
        self.remember(user, &name);
        Ok(name)
    }

    /// Give each member record without a name its display name, and each record the names of the people and the
    /// label-less channels its text mentions (`mentioned`), all lookups within `NAMES_DEADLINE`: authors first, then
    /// people, then channels, so a deadline that runs out costs the least wanted names (a header without a name shows an
    /// id; a channel mention shows its id as it did). The records are filled from what these lookups found, held here and
    /// not read back from the cache, whose 1,024 entries a large fill can push its own first names out of. What could
    /// not be looked up shows the id for an author and adds no entry for a mention. The channels come last, so the
    /// first answer no other channel escapes (`gives_up`) ends the fill's lookups.
    pub async fn fill_names(&self, records: &mut [ChatRecord]) {
        let mut ids: Vec<String> = Vec::new();
        let mentions: Vec<String> = records.iter().flat_map(|r| mentioned(&r.text)).collect();
        for id in records.iter().filter(|r| unnamed(r)).map(|r| r.author.id.clone()).chain(mentions) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        // Stable: channels (`C…`, `G…`) go last, the rest keep their order.
        ids.sort_by_key(|id| is_channel_id(id));
        let found: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
        let lookups = async {
            for id in &ids {
                match self.name_of(id, false).await {
                    Ok(name) => {
                        found.lock().unwrap().insert(id.clone(), name);
                    }
                    Err(e) if is_channel_id(id) && gives_up(&e) => break,
                    Err(_) => {}
                }
            }
        };
        let _ = tokio::time::timeout(self.deadlines.names, lookups).await;
        let found = found.into_inner().unwrap();
        name_records(records, |id| found.get(id).cloned());
        for r in records.iter_mut().filter(|r| unnamed(r)) {
            r.author.name = r.author.id.clone();
        }
    }

    /// The names the cache holds, with no request: a member's empty author name, and the `names` of the people the
    /// text mentions, replaced.
    pub fn cached_names(&self, records: &mut [ChatRecord]) {
        let names = self.names.lock().unwrap();
        name_records(records, |id| names.by_id.get(id).cloned());
    }

    /// The name the cache holds for an id (a member's, or a channel's once a lookup found it: a public one's, or a mapped
    /// one's), with no request.
    /// Every name in it went through `clean`, so none is empty or past `MAX_NAME`.
    pub fn cached_name(&self, id: &str) -> Option<String> {
        self.names.lock().unwrap().by_id.get(id).cloned()
    }

    /// The names of the configured channels, looked up so a later `cached_name` finds them: the distinct channel ids
    /// among `channels` (at most `MAX_CHANNELS`; a user or a direct-message id is no channel and is not asked for), one
    /// after another within the one `NAMES_DEADLINE` a fill has, and ended at the first answer no other channel escapes
    /// (`gives_up`). Each channel that got no name is returned with a fixed code (`ApiError::code`, or `timeout` for one the
    /// deadline cut off), for the caller to log; a name already cached is not asked for again.
    pub async fn channel_names(&self, channels: &[String]) -> Vec<(String, &'static str)> {
        let mut ids: Vec<&String> = Vec::new();
        for c in channels.iter().filter(|c| is_channel_id(c)) {
            if !ids.contains(&c) && ids.len() < MAX_CHANNELS {
                ids.push(c);
            }
        }
        let failed: Mutex<Vec<(String, &'static str)>> = Mutex::new(Vec::new());
        let lookups = async {
            for id in &ids {
                if let Err(e) = self.name_of(id, true).await {
                    failed.lock().unwrap().push((id.to_string(), e.code()));
                    if gives_up(&e) {
                        break;
                    }
                }
            }
        };
        let cut_off = tokio::time::timeout(self.deadlines.names, lookups).await.is_err();
        let mut failed = failed.into_inner().unwrap();
        if cut_off {
            let tried: Vec<String> = failed.iter().map(|(id, _)| id.clone()).collect();
            failed.extend(ids.iter().filter(|id| self.cached_name(id).is_none() && !tried.contains(id)).map(|id| (id.to_string(), "timeout")));
        }
        failed
    }

    /// A channel's name from `conversations.info`, cleaned. The one rule of which channels may be named: a mapped channel
    /// (`mapped`: the hub's owner mapped it to a row, the bot reads it and the drawer shows its messages) is named
    /// whether public or private; any other (a mention's) only when Slack says it is not private, since a reader may
    /// not be in a private one and sees its label or id. A mapped private channel's name is in the cache, so a mention of
    /// it shows the name too.
    async fn channel_name(&self, channel: &str, mapped: bool) -> Result<String, ApiError> {
        let facts = self.channel(channel).await?;
        (mapped || !facts.private).then_some(facts.name).ok_or(ApiError::Malformed)
    }

    async fn lookup(&self, user: &str, mapped: bool) -> Result<String, ApiError> {
        if !valid_id(user) {
            return Err(ApiError::BadRequest);
        }
        if is_channel_id(user) {
            return self.channel_name(user, mapped).await;
        }
        let v = self
            .read("users.info", &[("user", user.into())], MAX_BODY)
            .await?;
        let profile = v.pointer("/user/profile");
        let field = |holder: Option<&Value>, k: &str| {
            holder
                .and_then(|h| h.get(k))
                .and_then(Value::as_str)
                .map(|s| clean(s, MAX_NAME).0)
                .filter(|s| !s.is_empty())
        };
        field(profile, "display_name")
            .or_else(|| field(profile, "real_name"))
            .or_else(|| field(v.get("user"), "name"))
            .ok_or(ApiError::Malformed)
    }

    pub(crate) fn remember(&self, user: &str, name: &str) {
        let mut names = self.names.lock().unwrap();
        if !names.by_id.contains_key(user) {
            if names.by_id.len() >= MAX_NAMES {
                if let Some(oldest) = names.order.pop_front() {
                    names.by_id.remove(&oldest);
                }
            }
            names.order.push_back(user.into());
        }
        names.by_id.insert(user.into(), name.into());
    }

    fn url(&self, method: &str, params: &[(&str, String)]) -> Result<String, ApiError> {
        let mut url =
            Url::parse(&format!("{}{method}", self.base)).map_err(|_| ApiError::BadRequest)?;
        if !params.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in params {
                pairs.append_pair(k, v);
            }
        }
        Ok(url.into())
    }

    /// A rate limit's answer: reads wait out its retry header (the longest wait in force).
    fn rate_limited(&self, response: &Response) -> ApiError {
        let (retry_after, assumed) = retry_after(response);
        let until = Instant::now() + retry_after;
        let mut eligible = self.eligible.lock().unwrap();
        *eligible = Some(eligible.map_or(until, |t| t.max(until)));
        ApiError::Limited { retry_after, assumed }
    }

    /// One read, its response bounded by `max_body`: refused while a rate limit's wait is running, one request
    /// otherwise, and the answer is the JSON object with `ok: true` or an error.
    async fn read(
        &self,
        method: &str,
        params: &[(&str, String)],
        max_body: usize,
    ) -> Result<Value, ApiError> {
        let read = self.read_as(self.token.clone(), method, params, max_body).await;
        if let Err(ApiError::Slack(code)) = &read {
            self.refused(PostKind::Row, code);
        }
        read
    }

    /// `read` under `token`: a person's share reads its upload URL under the token that shares it.
    async fn read_as(
        &self,
        token: Secret,
        method: &str,
        params: &[(&str, String)],
        max_body: usize,
    ) -> Result<Value, ApiError> {
        if self.is_closed() {
            return Err(ApiError::Closed);
        }
        let wait = (*self.eligible.lock().unwrap())
            .and_then(|t| t.checked_duration_since(Instant::now()))
            .filter(|d| !d.is_zero());
        if let Some(retry_after) = wait {
            return Err(ApiError::Limited {
                retry_after,
                assumed: false,
            });
        }
        let request = Request::get(self.url(method, params)?, self.deadlines.read)
            .max_body(max_body)
            .bearer(token);
        let response = self.http.send(request).await.map_err(ApiError::Http)?;
        match response.status() {
            429 => return Err(self.rate_limited(&response)),
            200..=299 => {}
            other => return Err(ApiError::Status(other)),
        }
        let v: Value = serde_json::from_slice(response.body()).map_err(|_| ApiError::Malformed)?;
        match v.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(v),
            Some(false) => match error_code(&v) {
                Some("ratelimited") => Err(self.rate_limited(&response)),
                Some(code) => Err(ApiError::Slack(code.into())),
                None => Err(ApiError::Slack("unknown_error".into())),
            },
            None => Err(ApiError::Malformed),
        }
    }
}

/// Whether a `files.info` file is shared in `channel`: the channel is one of its `channels` or `groups`, or a key of
/// `shares.public` or `shares.private`.
fn shared_in(file: &Value, channel: &str) -> bool {
    let listed = |key: &str| {
        file.get(key).and_then(Value::as_array).is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(channel)))
    };
    let shared = |kind: &str| {
        file.pointer(&format!("/shares/{kind}")).and_then(Value::as_object).is_some_and(|m| m.contains_key(channel))
    };
    listed("channels") || listed("groups") || shared("public") || shared("private")
}

/// An answer's `Content-Type` when it is one of wire's `PICTURE_MIMES`, parameters dropped, lower case.
fn image_type(response: &Response) -> Option<String> {
    let kind = response.header("content-type")?.split(';').next()?.trim().to_ascii_lowercase();
    PICTURE_MIMES.contains(&kind.as_str()).then_some(kind)
}

/// An `auth.test` answer's `team_id`: `Malformed` when missing or not an id.
fn workspace_of(v: &Value) -> Result<&str, ApiError> {
    answered_id(v, "team_id").ok_or(ApiError::Malformed)
}

/// An `auth.test` answer's bot: `user_id` is required (`Malformed` without it), `bot_id` is kept when valid.
fn identity_of(v: &Value) -> Result<Identity, ApiError> {
    Ok(Identity {
        user_id: answered_id(v, "user_id").ok_or(ApiError::Malformed)?.into(),
        bot_id: answered_id(v, "bot_id").map(String::from),
        app_id: None,
    })
}

/// An `auth.test` answer's `team`, the workspace's name, read by [`name_at`].
fn name_of(v: &Value) -> Option<String> {
    name_at(v, "team")
}

/// A name Slack gave under `key`, its control characters dropped and trimmed; absent, not a string, empty, or past
/// `MAX_NAME` is none (words only: the window then shows an id, or words without the name).
fn name_at(v: &Value, key: &str) -> Option<String> {
    let (name, cut) = clean(v.get(key)?.as_str()?, MAX_NAME);
    (!name.is_empty() && !cut).then_some(name)
}

fn answered_id<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| valid_id(s))
}

/// Each unnamed member's author name, and each record's `names`, from `get`; what it lacks stays empty or has no entry.
fn name_records(records: &mut [ChatRecord], get: impl Fn(&str) -> Option<String>) {
    for r in records.iter_mut() {
        if unnamed(r) {
            r.author.name = get(&r.author.id).unwrap_or_default();
        }
        r.names = mentioned(&r.text)
            .into_iter()
            .filter_map(|id| get(&id).map(|name| ChatName { name, id }))
            .collect();
    }
}

/// A member's record with an author id and no name yet.
fn unnamed(r: &ChatRecord) -> bool {
    r.origin == Origin::Member && r.author.name.is_empty() && !r.author.id.is_empty()
}

/// A channel entry of a list: `None` for one with no valid channel id or no name left after cleaning.
fn listed(raw: &Value) -> Option<Channel> {
    let id = raw.get("id").and_then(Value::as_str).filter(|id| valid_id(id) && is_channel_id(id))?;
    channel_facts(raw, id.into())
}

/// The facts of a channel object, under `id`; `None` for one whose name is missing or empty once cleaned.
fn channel_facts(raw: &Value, id: String) -> Option<Channel> {
    let name = raw.get("name").and_then(Value::as_str).map(|s| clean(s, MAX_NAME).0).filter(|s| !s.is_empty())?;
    Some(Channel {
        id,
        name,
        private: raw.get("is_private").and_then(Value::as_bool) != Some(false),
        member: raw.get("is_member").and_then(Value::as_bool) == Some(true),
    })
}

/// Whether an id names a channel (`C…`, `G…`), as a person's begins `U` or `W`.
fn is_channel_id(id: &str) -> bool {
    id.starts_with(['C', 'G'])
}

/// An answer no other channel escapes: the scope or the token is missing or revoked, or Slack is limiting reads. The
/// rest of a fill's channels would fail or earn a wait the same way.
fn gives_up(e: &ApiError) -> bool {
    const CODES: [&str; 6] =
        ["missing_scope", "not_authed", "invalid_auth", "token_revoked", "account_inactive", "not_allowed_token_type"];
    match e {
        ApiError::Limited { .. } => true,
        ApiError::Slack(code) => CODES.contains(&code.as_str()),
        _ => false,
    }
}

/// The distinct ids of the `<@U…>` and `<@W…>` tokens in a text, then of the `<#C…>` and `<#G…>` tokens with no label,
/// together at most `MAX_MENTIONS`, each kind in the text's order: a full text gives up channels before people. A token
/// a byte cut left open has no id.
fn mentioned(text: &str) -> Vec<String> {
    let (mut people, mut channels): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for seg in text_segments(text) {
        let TextSegment::Markup(t) = seg else { continue };
        let Some(inner) = t.strip_prefix('<').and_then(|t| t.strip_suffix('>')) else { continue };
        let (head, label) = inner.split_once('|').unwrap_or((inner, ""));
        let id = head.get(1..).unwrap_or_default();
        let into = match head.get(..1) {
            Some("@") if id.starts_with(['U', 'W']) => &mut people,
            Some("#") if label.is_empty() && is_channel_id(id) => &mut channels,
            _ => continue,
        };
        if valid_id(id) && !into.iter().any(|i| i == id) && into.len() < MAX_MENTIONS {
            into.push(id.to_string());
        }
    }
    people.extend(channels);
    people.truncate(MAX_MENTIONS);
    people
}

/// A history or replies answer's `messages` (at most `capacity`), `has_more` and `is_limited`; anything else is
/// `Malformed`.
fn page_parts(v: &Value, capacity: usize) -> Result<(&Vec<Value>, bool, bool), ApiError> {
    let messages = v
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| m.len() <= capacity)
        .ok_or(ApiError::Malformed)?;
    let has_more = v
        .get("has_more")
        .and_then(Value::as_bool)
        .ok_or(ApiError::Malformed)?;
    let limited = match v.get("is_limited") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(ApiError::Malformed),
    };
    Ok((messages, has_more, limited))
}

/// The retry header, whatever its case, in seconds: capped at `MAX_RETRY`. Missing or invalid is the conservative
/// `ASSUMED_RETRY`, flagged.
fn retry_after(response: &Response) -> (Duration, bool) {
    let parsed = response
        .header("retry-after")
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse::<u64>().ok());
    match parsed {
        Some(secs) => (Duration::from_secs(secs).min(MAX_RETRY), false),
        None => (ASSUMED_RETRY, true),
    }
}

/// A post's author name: as `clean` leaves it, within `MAX_NAME`.
fn author_ok(name: &str) -> bool {
    !name.is_empty() && name == clean(name, MAX_NAME).0 && name.len() <= MAX_NAME
}

/// A failure before a share as the code its refusal carries: the HTTP core's, Slack's, a status as `http_<code>`, or
/// the call's own.
fn refusal(e: &ApiError) -> String {
    match e {
        ApiError::Http(h) => h.code().into(),
        ApiError::Slack(code) => code.clone(),
        ApiError::Status(s) => format!("http_{s}"),
        other => other.code().into(),
    }
}

/// Slack's `error` code, only when it is `[a-z0-9_.]{1,64}`: its text is never copied.
fn error_code(v: &Value) -> Option<&str> {
    v.get("error").and_then(Value::as_str).filter(|c| {
        (1..=64).contains(&c.len())
            && c.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
    })
}

/// A mutation's answer. It is `Refused` or `Limited` only when Slack said so (a 429 or other non-5xx failure, or
/// `ok: false` with a known, non-transient code); `Unknown` in every other case that is not an acknowledgement.
enum Ack {
    /// `ok: true`, with the body.
    Done(Value),
    Refused(String),
    Limited {
        retry_after: Duration,
        assumed: bool,
    },
    Unknown,
}

fn acknowledgement(result: Result<Response, HttpError>) -> Ack {
    let response = match result {
        Ok(response) => response,
        // Nothing left this machine.
        Err(e @ (HttpError::Busy | HttpError::Invalid | HttpError::Destination)) => {
            return Ack::Refused(e.code().into())
        }
        // Anything after a connection may have reached Slack: a timeout, a dropped answer, a redirect.
        Err(_) => return Ack::Unknown,
    };
    let limited = || {
        let (retry_after, assumed) = retry_after(&response);
        Ack::Limited {
            retry_after,
            assumed,
        }
    };
    match response.status() {
        429 => return limited(),
        200..=299 => {}
        500..=599 => return Ack::Unknown,
        other => return Ack::Refused(format!("http_{other}")),
    }
    let Ok(v) = serde_json::from_slice::<Value>(response.body()) else {
        return Ack::Unknown;
    };
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => Ack::Done(v),
        Some(false) => match error_code(&v) {
            Some("ratelimited") => limited(),
            Some("internal_error" | "fatal_error" | "request_timeout" | "service_unavailable")
            | None => Ack::Unknown,
            Some(code) => Ack::Refused(code.into()),
        },
        None => Ack::Unknown,
    }
}

/// A post's answer: the acknowledgement, read for its `ts`; an `ok` with none is `Unknown`.
fn post_outcome(result: Result<Response, HttpError>) -> PostOutcome {
    match acknowledgement(result) {
        Ack::Done(v) => v
            .get("ts")
            .and_then(Value::as_str)
            .and_then(|t| Ts::parse(t).ok())
            .map_or(PostOutcome::Unknown, PostOutcome::Posted),
        Ack::Refused(code) => PostOutcome::Refused(code),
        Ack::Limited { retry_after, assumed } => PostOutcome::Limited { retry_after, assumed },
        Ack::Unknown => PostOutcome::Unknown,
    }
}

/// A share's answer: the acknowledgement, read for `files[0].id` (an `ok` with none is `Unknown`) and the share's `ts`
/// in `channel`, public or private, which Slack's answer can lack.
fn share_outcome(result: Result<Response, HttpError>, channel: &str) -> FileOutcome {
    match acknowledgement(result) {
        Ack::Done(v) => {
            let shared = v.pointer("/files/0");
            let Some(file) = shared.and_then(|f| answered_id(f, "id")) else {
                return FileOutcome::Unknown;
            };
            let ts = ["public", "private"]
                .iter()
                .find_map(|kind| shared?.get("shares")?.get(*kind)?.get(channel)?.get(0)?.get("ts")?.as_str())
                .and_then(|t| Ts::parse(t).ok());
            FileOutcome::Posted { file: file.into(), ts }
        }
        Ack::Refused(code) => FileOutcome::Refused(code),
        Ack::Limited { retry_after, assumed } => FileOutcome::Limited { retry_after, assumed },
        Ack::Unknown => FileOutcome::Unknown,
    }
}
