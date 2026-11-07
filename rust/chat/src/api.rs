//! The Slack Web API calls the first chat stage needs: `auth.test`, channel history and thread replies (paged),
//! `chat.postMessage` under a custom author, and author names through a bounded cache. Every call goes out through
//! `http` and comes back as the wire record through `normalize`; there is no second record and no second HTTP path.
//! No socket, no file call, no configuration: the caller hands in the `Secret`.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::Url;
use serde_json::{json, Value};
use sot_protocol::chat::{ChatRecord, Origin, Ts};
use tokio::time::Instant;

use crate::http::{Http, HttpError, Request, Response, Secret};
use crate::normalize::{clean, normalize, ts_key, valid_id, Context, Normalized, MAX_NAME, MAX_TEXT};

/// A read (`auth.test`, history, replies, `users.info`): Slack answers in well under a second, so fifteen seconds
/// covers a slow link and still frees a drawer waiting on a stalled page.
pub const READ_DEADLINE: Duration = Duration::from_secs(15);
/// A post is never resent, so its wait is longer than a read's: giving up early turns an acknowledgement still on its
/// way into `Unknown` for the person to resolve.
pub const POST_DEADLINE: Duration = Duration::from_secs(30);
/// All the name lookups for one page together: names are decoration, so the page is shown with ids after this.
pub const NAMES_DEADLINE: Duration = Duration::from_secs(10);
pub use sot_protocol::ops::{MAX_CURSOR, MAX_PAGE};
/// Author names kept; the oldest entry leaves first.
pub const MAX_NAMES: usize = 1024;
/// Reads wait this long after a rate limit whose retry header is missing or invalid.
pub const ASSUMED_RETRY: Duration = Duration::from_secs(30);
/// A hostile or mistaken retry header cannot silence reads longer than this.
pub const MAX_RETRY: Duration = Duration::from_secs(sot_protocol::ops::MAX_RETRY_SECS);

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

/// The bot's identity from `auth.test`: normalization decides which messages are this app's own by it alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub user_id: String,
    pub bot_id: Option<String>,
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

/// Whose post this is. The kind is carried in the metadata and the username suffix, as `normalize` reads them.
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

#[derive(Clone, Copy)]
struct Deadlines {
    read: Duration,
    post: Duration,
    names: Duration,
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
    workspace: String,
    base: String,
    deadlines: Deadlines,
    own: Mutex<Option<Identity>>,
    /// Reads are not eligible before this instant (the last rate limit's retry header).
    eligible: Mutex<Option<Instant>>,
    names: Mutex<Names>,
    closed: AtomicBool,
}

impl Api {
    /// `workspace` is the Slack team id the token must belong to.
    pub fn new(http: Http, token: Secret, workspace: &str) -> Result<Api, ApiError> {
        if !valid_id(workspace) {
            return Err(ApiError::BadRequest);
        }
        Ok(Api {
            http,
            token,
            workspace: workspace.into(),
            base: BASE.into(),
            deadlines: Deadlines {
                read: READ_DEADLINE,
                post: POST_DEADLINE,
                names: NAMES_DEADLINE,
            },
            own: Mutex::new(None),
            eligible: Mutex::new(None),
            names: Mutex::new(Names::default()),
            closed: AtomicBool::new(false),
        })
    }

    /// Refuse every later call, on every handle. A call already sent runs to its own deadline.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// The fixture's logical base URL; the product has no endpoint override.
    #[cfg(test)]
    pub(crate) fn with_base(mut self, base: String) -> Api {
        self.base = base;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_deadlines(mut self, read: Duration, post: Duration, names: Duration) -> Api {
        self.deadlines = Deadlines { read, post, names };
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

    /// The bot identity, which history needs. The token must belong to the configured workspace.
    pub async fn auth_test(&self) -> Result<Identity, ApiError> {
        let v = self.read("auth.test", &[]).await?;
        let id = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| valid_id(s));
        if id("team_id").ok_or(ApiError::Malformed)? != self.workspace {
            return Err(ApiError::WrongWorkspace);
        }
        let identity = Identity {
            user_id: id("user_id").ok_or(ApiError::Malformed)?.into(),
            bot_id: id("bot_id").map(String::from),
        };
        *self.own.lock().unwrap() = Some(identity.clone());
        Ok(identity)
    }

    /// One page of a channel's history (newest first, paging backward) or of a thread's replies (paging forward).
    /// `cont` continues a page this client returned for this same query. The page is whole or an error.
    pub async fn history(
        &self,
        query: &HistoryQuery,
        cont: Option<&Continuation>,
    ) -> Result<Page, ApiError> {
        let own = self
            .own
            .lock()
            .unwrap()
            .clone()
            .ok_or(ApiError::NoIdentity)?;
        let sent = cont.map(|c| c.unwrap(&self.workspace, query)).transpose()?;
        let mut params = vec![
            ("channel", query.channel.clone()),
            ("limit", query.limit.to_string()),
            ("include_all_metadata", "true".into()),
        ];
        let method = match &query.thread {
            Some(parent) => {
                params.push(("ts", parent.as_str().into()));
                "conversations.replies"
            }
            None => "conversations.history",
        };
        if let Some(after) = &query.bounds.after {
            params.push(("oldest", after.as_str().into()));
        }
        match &query.bounds.upper {
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
        if let Some(cursor) = &sent {
            params.push(("cursor", cursor.clone()));
        }
        let v = self.read(method, &params).await?;

        let messages = v
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(ApiError::Malformed)?;
        if messages.len() > query.capacity() {
            return Err(ApiError::Malformed);
        }
        let has_more = v
            .get("has_more")
            .and_then(Value::as_bool)
            .ok_or(ApiError::Malformed)?;
        let limited = match v.get("is_limited") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(ApiError::Malformed),
        };
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

    /// Post once under a custom author. `thread`: a parent's `ts`, to reply. Input the post cannot carry is refused
    /// before anything is sent (`Err`); from there the result is a `PostOutcome`, and no path resends.
    pub async fn post(
        &self,
        channel: &str,
        thread: Option<&str>,
        kind: PostKind,
        name: &str,
        text: &str,
    ) -> Result<PostOutcome, ApiError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ApiError::Closed);
        }
        let name_ok = !name.is_empty() && name == clean(name, MAX_NAME).0 && name.len() <= MAX_NAME;
        let thread = thread
            .map(|t| Ts::parse(t).map_err(|_| ApiError::BadBound))
            .transpose()?;
        if !valid_id(channel) || !name_ok || text.is_empty() || text.len() > MAX_TEXT {
            return Err(ApiError::BadRequest);
        }
        let mut body = json!({
            "channel": channel,
            "text": text,
            "username": format!("{name} ({} via Ship of Tools)", kind.word()),
            "metadata": {
                "event_type": "sot_chat_post",
                "event_payload": {"kind": kind.word(), "name": name},
            },
        });
        if let Some(parent) = thread {
            body["thread_ts"] = parent.as_str().into();
        }
        let request = Request::post(
            self.url("chat.postMessage", &[])?,
            self.deadlines.post,
            JSON,
            body.to_string().into_bytes(),
        )
        .bearer(self.token.clone());
        Ok(post_outcome(self.http.send(request).await))
    }

    /// A member's display name: `display_name`, then `real_name`, then `name`, from the cache or `users.info`. Any
    /// failure gives the id, and a failure is not remembered.
    pub async fn author_name(&self, user: &str) -> String {
        if let Some(name) = self.names.lock().unwrap().by_id.get(user) {
            return name.clone();
        }
        match self.lookup(user).await {
            Ok(name) => {
                self.remember(user, &name);
                name
            }
            Err(_) => user.to_string(),
        }
    }

    /// Give each member record without a name its display name, all lookups within `NAMES_DEADLINE`; what could not
    /// be looked up shows the id.
    pub async fn fill_names(&self, records: &mut [ChatRecord]) {
        let mut ids: Vec<String> = Vec::new();
        for r in records.iter() {
            let wanted = r.origin == Origin::Member && r.author.name.is_empty();
            if wanted && !r.author.id.is_empty() && !ids.contains(&r.author.id) {
                ids.push(r.author.id.clone());
            }
        }
        let lookups = async {
            for id in &ids {
                self.author_name(id).await;
            }
        };
        let _ = tokio::time::timeout(self.deadlines.names, lookups).await;
        let names = self.names.lock().unwrap();
        for r in records.iter_mut() {
            if r.origin == Origin::Member && r.author.name.is_empty() && !r.author.id.is_empty() {
                r.author.name = names
                    .by_id
                    .get(&r.author.id)
                    .cloned()
                    .unwrap_or_else(|| r.author.id.clone());
            }
        }
    }

    async fn lookup(&self, user: &str) -> Result<String, ApiError> {
        if !valid_id(user) {
            return Err(ApiError::BadRequest);
        }
        let v = self.read("users.info", &[("user", user.into())]).await?;
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

    /// One read: refused while a rate limit's wait is running, one request otherwise, and the answer is the JSON
    /// object with `ok: true` or an error.
    async fn read(&self, method: &str, params: &[(&str, String)]) -> Result<Value, ApiError> {
        if self.closed.load(Ordering::SeqCst) {
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
        let request =
            Request::get(self.url(method, params)?, self.deadlines.read).bearer(self.token.clone());
        let response = self.http.send(request).await.map_err(ApiError::Http)?;
        let limited = |api: &Api| {
            let (retry_after, assumed) = retry_after(&response);
            let until = Instant::now() + retry_after;
            let mut eligible = api.eligible.lock().unwrap();
            *eligible = Some(eligible.map_or(until, |t| t.max(until)));
            ApiError::Limited {
                retry_after,
                assumed,
            }
        };
        match response.status() {
            429 => return Err(limited(self)),
            200..=299 => {}
            other => return Err(ApiError::Status(other)),
        }
        let v: Value = serde_json::from_slice(response.body()).map_err(|_| ApiError::Malformed)?;
        match v.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(v),
            Some(false) => match error_code(&v) {
                Some("ratelimited") => Err(limited(self)),
                Some(code) => Err(ApiError::Slack(code.into())),
                None => Err(ApiError::Slack("unknown_error".into())),
            },
            None => Err(ApiError::Malformed),
        }
    }
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
fn post_outcome(result: Result<Response, HttpError>) -> PostOutcome {
    let response = match result {
        Ok(response) => response,
        // Nothing left this machine.
        Err(e @ (HttpError::Busy | HttpError::Invalid | HttpError::Destination)) => {
            return PostOutcome::Refused(e.code().into())
        }
        // Anything after a connection may have reached Slack: a timeout, a dropped answer, a redirect.
        Err(_) => return PostOutcome::Unknown,
    };
    let limited = || {
        let (retry_after, assumed) = retry_after(&response);
        PostOutcome::Limited {
            retry_after,
            assumed,
        }
    };
    match response.status() {
        429 => return limited(),
        200..=299 => {}
        500..=599 => return PostOutcome::Unknown,
        other => return PostOutcome::Refused(format!("http_{other}")),
    }
    let Ok(v) = serde_json::from_slice::<Value>(response.body()) else {
        return PostOutcome::Unknown;
    };
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => v
            .get("ts")
            .and_then(Value::as_str)
            .and_then(|t| Ts::parse(t).ok())
            .map_or(PostOutcome::Unknown, PostOutcome::Posted),
        Some(false) => match error_code(&v) {
            Some("ratelimited") => limited(),
            Some("internal_error" | "fatal_error" | "request_timeout" | "service_unavailable")
            | None => PostOutcome::Unknown,
            Some(code) => PostOutcome::Refused(code.into()),
        },
        None => PostOutcome::Unknown,
    }
}
