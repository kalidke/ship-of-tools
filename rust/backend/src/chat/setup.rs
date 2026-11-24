//! The setup ops: `chat.connect`, which checks three tokens with Slack, saves them as their workspace's secrets
//! (adding a workspace, or giving one new tokens) and reloads the chat from its files with no daemon restart;
//! `chat.channels`, the picker's list of one workspace; `chat.pair`, which maps a row's folder to a channel of one
//! workspace or unmaps it, rewrites `chat.toml` and reloads; `chat.row_pair`, a session's own pairing of a folder, which
//! makes the same edit, write and reload through the same core; and `chat.workspaces`, the connected workspaces. The first
//! three are jobs taken from a window's connection only (a row's agent has no use for them, and no token goes on a
//! command line); `chat.row_pair` is any connection's, attributed by its `by`; `chat.workspaces` is a read any connection
//! may make. The edit of a file and the reload that follows it hold the setup lock together, so two pairings and a connect
//! never lose one another's change. A token is a `Secret` from its decode on: no log line, reply or error here carries a
//! byte of one, and no request payload is logged.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use sot_chat::api::{self as chat_api, Api, ApiError, BotApp, Channel, Given, Identified, JoinOutcome, TOKEN_REFUSALS};
use sot_chat::config::{self, Config, ConfigError, PairError, Pairer, Resolution, Secrets, Writers};
use sot_chat::http::{Http, Secret};
use sot_chat::service::{Service, APP_TOKEN_REFUSED, BOT_TOKEN_REFUSED, WRONG_APP};
use sot_chat::socket::SocketError;
use sot_platform::host::env::Env;
use sot_protocol::chat::Ts;
use sot_protocol::ops::{
    op, ChatChannel, ChatChannelInfo, ChatChannelsReq, ChatChannelsRes, ChatCode, ChatConnectReq, ChatConnectRes, ChatOtherApp,
    ChatPairOutcome, ChatPairReq, ChatPairRes, ChatPairing, ChatRowPairReq, ChatRowPairRes, ChatUnconfigured, ChatUnread, ChatWorkspacesReq,
    ChatWorkspacesRes, MAX_WATCHED, MAX_WORKSPACES,
};
use sot_protocol::reply::HandlerOutput;

use super::ops::{code_of, decode, is_bound, refuse, refuse_with, reply};
use super::{canon_of, current_row, is_hub, Chat, Status};
use crate::rows::Workspaces;

type Checked<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

/// The connect's Slack checks: the bot token's identity with no workspace expected (and the user token's, by the same
/// `identify`), then the app token's one `apps.connections.open` and, by its one probe socket, the Slack app that token
/// belongs to, and the bot's app (`bots.info` under the bot token, for the bot's id). The
/// product's reach Slack ([`Verifier::product`]); a test's reach the fake.
pub(crate) struct Verifier {
    pub(crate) identify: Box<dyn Fn(Secret) -> Checked<Identified, ApiError> + Send + Sync>,
    pub(crate) app: Box<dyn Fn(Secret) -> Checked<Option<String>, SocketError> + Send + Sync>,
    pub(crate) bot_app: Box<dyn Fn(Secret, String) -> Checked<BotApp, ApiError> + Send + Sync>,
}

impl Verifier {
    /// `api::identify`, `socket::check_app_token` and `api::bot_app_of` through one client, made at the first check, so
    /// every connect's calls share its bound on operations at once (`http::MAX_OPERATIONS`); the probe socket is dialed
    /// through the same client.
    pub(crate) fn product() -> Verifier {
        let client = Arc::new(std::sync::OnceLock::new());
        let one = move || client.get_or_init(Http::new).clone();
        let (other, third) = (one.clone(), one.clone());
        Verifier {
            identify: Box::new(move |bot| {
                let http = one();
                Box::pin(async move { sot_chat::api::identify(&http.map_err(ApiError::Http)?, bot).await })
            }),
            app: Box::new(move |app| {
                let http = other();
                Box::pin(async move { sot_chat::socket::check_app_token(&http.map_err(SocketError::Http)?, app).await })
            }),
            bot_app: Box::new(move |token, bot| {
                let http = third();
                Box::pin(async move { sot_chat::api::bot_app_of(&http.map_err(ApiError::Http)?, token, &bot).await })
            }),
        }
    }

    /// Every check against the fake Slack's base.
    #[cfg(test)]
    pub(crate) fn at(fake: &sot_chat::fake_slack::FakeSlack) -> Verifier {
        let (http, base) = (fake.http(), fake.url("slack.com", "/api/"));
        let (h, b) = (http.clone(), base.clone());
        let (h3, b3) = (http.clone(), base.clone());
        Verifier {
            identify: Box::new(move |bot| {
                let (http, base) = (h.clone(), b.clone());
                Box::pin(async move { sot_chat::api::identify_for_test(&http, bot, base).await })
            }),
            app: Box::new(move |app| {
                let (http, base) = (http.clone(), base.clone());
                Box::pin(async move { sot_chat::socket::check_app_token_for_test(&http, app, &base).await })
            }),
            bot_app: Box::new(move |token, bot| {
                let (http, base) = (h3.clone(), b3.clone());
                Box::pin(async move { sot_chat::api::bot_app_of_for_test(&http, token, &bot, base).await })
            }),
        }
    }

    /// Checks that never reach Slack, for a chat no test connects.
    #[cfg(test)]
    pub(crate) fn none() -> Verifier {
        Verifier {
            identify: Box::new(|_| Box::pin(async { Err(ApiError::Closed) })),
            app: Box::new(|_| Box::pin(async { Err(SocketError::BadRequest) })),
            bot_app: Box::new(|_, _| Box::pin(async { Err(ApiError::Closed) })),
        }
    }
}

/// Why the hub's chat is off when a connect fixes it: no `chat.toml` or no secrets, secrets the load refused, a start
/// that failed, a service that ended on its own (an app token of another app, a token Slack refused). A refused `chat.toml` is not one (a connect never rewrites it), nor is any other status; nor is a running
/// chat that holds no user token, which only a person's post cares about (its refusal says so).
pub(crate) fn unconfigured(status: Status) -> Option<ChatUnconfigured> {
    match status {
        Status::Refused(ConfigError::Missing) | Status::Secrets(ConfigError::Missing) => Some(ChatUnconfigured::NotConnected),
        Status::Secrets(_) => Some(ChatUnconfigured::TokensRefused),
        Status::Failed(WRONG_APP) => Some(ChatUnconfigured::WrongApp),
        Status::Failed(APP_TOKEN_REFUSED | BOT_TOKEN_REFUSED) => Some(ChatUnconfigured::TokensRefused),
        Status::Failed(_) => Some(ChatUnconfigured::StartFailed),
        _ => None,
    }
}

/// `chat.connect`: three tokens checked with Slack, saved, and the chat reloaded from its files. Tokens Slack tells to be
/// two Slack apps' (the bot token's and the app-level token's) are refused `mixed_apps`, whatever the request says; tokens
/// not known to be the Slack app their workspace runs or starts with now (another app's, or an app Slack did not tell on
/// either side) are refused `other_app` unless the request says `replace_app`; nothing is written or restarted for either.
/// The order and its codes are `rust/chat/CLAUDE.md` guarantee 31's. Nothing here logs on the blocking pool: the writes
/// answer their reason as a value, and this task logs it.
pub(crate) async fn connect(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces, window: bool) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_CONNECT;
    let req = match (window, serde_json::from_value::<ChatConnectReq>(payload)) {
        (true, Ok(r)) => r,
        _ => return Ok(refuse(req_id, NAME, ChatCode::BadRequest)),
    };
    if chat.closed_now() {
        return Ok(refuse(req_id, NAME, ChatCode::Unavailable));
    }
    let env = chat.env.clone();
    if !tokio::task::spawn_blocking(move || is_hub(&env)).await.unwrap_or(false) {
        return Ok(refuse(req_id, NAME, ChatCode::Unavailable));
    }
    // A connect with no user token (an older window's) is refused before any check: nothing is saved for it.
    let Some(user_token) = req.user_token.as_ref() else { return Ok(refuse(req_id, NAME, ChatCode::BadRequest)) };
    let Ok(bot) = config::bot_token(req.bot_token.expose()) else { return Ok(refuse(req_id, NAME, ChatCode::BotTokenRefused)) };
    let Ok(user) = config::user_token(user_token.expose()) else { return Ok(refuse(req_id, NAME, ChatCode::UserTokenRefused)) };
    let Ok(app) = config::app_token(req.app_token.expose()) else { return Ok(refuse(req_id, NAME, ChatCode::AppTokenRefused)) };
    let replace = req.replace_app;
    drop(req);
    // Only Slack's answers that name the token refuse it; a rate limit, an outage or a transport failure is `failed`.
    let (workspace, name, bot_id) = match (chat.verify.identify)(bot.clone()).await {
        Ok(id) => (id.workspace, id.name, id.own.bot_id),
        Err(ApiError::Slack(code)) if TOKEN_REFUSALS.contains(&code.as_str()) => return Ok(refuse(req_id, NAME, ChatCode::BotTokenRefused)),
        Err(e) => {
            tracing::warn!(code = e.code(), "project chat: a connect could not check the bot token");
            return Ok(refuse(req_id, NAME, ChatCode::Failed));
        }
    };
    // The same check of the user token, which must name the bot's workspace: a person's post goes there.
    match (chat.verify.identify)(user.clone()).await {
        Ok(id) if id.workspace == workspace => {}
        Ok(_) => return Ok(refuse(req_id, NAME, ChatCode::UserTokenRefused)),
        Err(ApiError::Slack(code)) if TOKEN_REFUSALS.contains(&code.as_str()) => return Ok(refuse(req_id, NAME, ChatCode::UserTokenRefused)),
        Err(e) => {
            tracing::warn!(code = e.code(), "project chat: a connect could not check the user token");
            return Ok(refuse(req_id, NAME, ChatCode::Failed));
        }
    }
    // The app-level token's check, which also tells its Slack app, beside the question of the bot token's app: the two
    // run at once on every connect, the question within one read's deadline and the check within two (its open, then its
    // probe socket). No bot id, or any failure of the question (its
    // deadline included), leaves the bot's app untold; a probe socket that tells nothing leaves the app-level token's.
    let asked = async {
        let told = (chat.verify.bot_app)(bot.clone(), bot_id?).await;
        told.inspect_err(|e| tracing::warn!(code = e.code(), "project chat: a connect could not tell its tokens' Slack app")).ok()
    };
    let (of_token, of_bot) = match tokio::join!((chat.verify.app)(app.clone()), asked) {
        (Ok(of_token), of_bot) => (of_token, of_bot),
        (Err(SocketError::Rejected), _) => return Ok(refuse(req_id, NAME, ChatCode::AppTokenRefused)),
        (Err(e), _) => {
            tracing::warn!(code = e.code(), "project chat: a connect could not check the app-level token");
            return Ok(refuse(req_id, NAME, ChatCode::Failed));
        }
    };
    // Two apps told and not the same one: such a set is never written, whatever the request says and whatever runs.
    if of_bot.as_ref().zip(of_token.as_ref()).is_some_and(|(b, t)| b.id != *t) {
        tracing::info!(workspace, "project chat: a connect's app-level token is another Slack app's than its bot token's; nothing was saved");
        return Ok(refuse(req_id, NAME, ChatCode::MixedApps));
    }
    // The tokens' one app is told only when both sides told it.
    let new_app = of_bot.filter(|_| of_token.is_some());
    let held = chat.setup().await;
    // Under the lock, against what the workspace runs or starts with now (a start waited for first, and a running chat
    // whose app was never told asked for it once): unless the window says to replace it, tokens that are not known to be
    // that app's write nothing and restart nothing, and the refusal says what the window asks with.
    let at_stake = if replace { None } else { chat.app_at_stake(&workspace).await };
    // A retirement that began while Slack was asked or a start waited for: nothing is written for a daemon that is going away.
    if chat.closed_now() {
        return Ok(refuse(req_id, NAME, ChatCode::Unavailable));
    }
    if let Some(now) = at_stake {
        if !now.as_ref().zip(new_app.as_ref()).is_some_and(|(now, new)| now.id == new.id) {
            // Told to be another app only when both are told: an app not told on either side is the weaker claim.
            let known = now.is_some() && new_app.is_some();
            let (app_id, app_name) = now.map_or((None, None), |a| (Some(a.id), a.name));
            let body = ChatOtherApp { workspace: Some(workspace.clone()), name, app_id, app_name, new_app_known: known, new_app_name: new_app.filter(|_| known).and_then(|n| n.name) };
            tracing::info!(workspace, known, "project chat: a connect's tokens are not known to be the connected Slack app's; nothing was saved");
            return refuse_with(req_id, NAME, ChatCode::OtherApp, &body);
        }
    }
    let (env, named) = (chat.env.clone(), workspace.clone());
    let saved = tokio::task::spawn_blocking(move || save_tokens(&env, &named, &bot, &user, &app)).await;
    if let Err((code, why)) = saved.unwrap_or(Err((ChatCode::NotSaved, Some("the write's task ended".into())))) {
        if let Some(why) = why {
            tracing::warn!(%why, "project chat: a connect saved nothing");
        }
        return Ok(refuse(req_id, NAME, code));
    }
    // Before the reload: the workspace's mapped channels and the names its running app has for them, which a new app
    // outside a private channel cannot read.
    let mapped = mapped_channels(chat, &workspace);
    chat.reload_for(&held, rows, &workspace).await;
    drop(held);
    chat.remember_name(&workspace, name.clone());
    let status = chat.status_of(&workspace);
    tracing::info!(workspace, %status, "project chat: a window's connect saved the tokens and reloaded");
    // The reply waits on no Slack read: which of them the app now running cannot read is a task's, told by `chat.workspaces`.
    find_unread(chat, &workspace, mapped);
    reply(req_id, NAME, &ChatConnectRes { workspace, name })
}

/// How long the reads of a connect's task (`find_unread`) may take in all.
pub(crate) const UNREAD_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The channels `workspace`'s mappings name, once each, with the name its running app's cache holds for each.
fn mapped_channels(chat: &Arc<Chat>, workspace: &str) -> Vec<(String, Option<String>)> {
    let api = chat.service_of(workspace).and_then(|s| s.api().ok());
    let config = chat.config();
    let Some(Ok(config)) = config.as_ref() else { return Vec::new() };
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for (_, w, channel) in config.mappings() {
        if w == workspace && !out.iter().any(|(c, _)| c == channel) {
            out.push((channel.to_string(), api.as_ref().and_then(|a| a.cached_name(channel))));
        }
    }
    out
}

/// Starts, on a task of its own, the reads that find which of `channels` (each with the name the app before had for it)
/// the Slack app `workspace` runs now cannot read, kept for `chat.workspaces` against that app's service
/// (`Chat::keep_unread`); nothing when the workspace does not run. The task holds the chat and the service only weakly, so
/// neither waits on it. A stopped service refuses the task's next read at once; a read already sent runs to `wait`.
fn find_unread(chat: &Arc<Chat>, workspace: &str, channels: Vec<(String, Option<String>)>) {
    let Some(service) = chat.service_of(workspace) else { return };
    let Ok(api) = service.api() else { return };
    let (held, of, workspace, wait) = (Arc::downgrade(chat), Arc::downgrade(&service), workspace.to_string(), chat.unread_wait());
    drop(service);
    drop(tokio::spawn(async move {
        let found = unread(&api, channels, wait).await;
        // A test's hold point, keyed by the chat's address: the reads have ended and nothing is kept yet.
        sot_platform::hold_point_async!("chat.unread.read", held.as_ptr() as usize);
        if let (Some(found), Some(chat)) = (found, held.upgrade()) {
            chat.keep_unread(&workspace, &of, found);
        }
        // A test's hold point, keyed by the chat's address: the reads have ended, and what they found is kept or not.
        sot_platform::hold_point_async!("chat.unread.found", held.as_ptr() as usize);
    }));
}

/// `find_unread`'s reads: one `Api::channel` each, in order, all within `wait`, ending once `MAX_WATCHED` are found. A
/// channel Slack says the app is not in, or does not show it (`channel_not_found`, its answer for a private channel the
/// app is not in), is one, named as the app before had it, else as Slack names it (each name already cut to `MAX_NAME`).
/// Known when Slack answered every channel in time; a read `wait` cut off, or answered any other way (a rate limit, an
/// outage), leaves it known only when it found one, so an empty list always means the app reads every mapped channel.
async fn unread(api: &Api, channels: Vec<(String, Option<String>)>, wait: std::time::Duration) -> Option<Vec<ChatUnread>> {
    let found = std::sync::Mutex::new(Vec::new());
    let reads = async {
        let mut whole = true;
        for (id, name) in channels {
            let name = match api.channel(&id).await {
                Ok(c) if c.member => continue,
                Ok(c) => name.or(Some(c.name)),
                Err(ApiError::Slack(code)) if code == "channel_not_found" => name,
                Err(_) => {
                    whole = false;
                    continue;
                }
            };
            let Ok(id) = ChatChannel::try_from(id) else { continue };
            let mut found = found.lock().unwrap_or_else(|e| e.into_inner());
            found.push(ChatUnread { id, name });
            if found.len() == MAX_WATCHED {
                break;
            }
        }
        whole
    };
    let whole = tokio::time::timeout(wait, reads).await.unwrap_or(false);
    let found = found.into_inner().unwrap_or_else(|e| e.into_inner());
    (whole || !found.is_empty()).then_some(found)
}

/// Why the connect's writes did not happen: the reply's code and, for the caller to log, a fixed reason with the load's
/// refusal or the error's kind, never a value of the files.
type Unsaved = (ChatCode, Option<String>);

/// The connect's writes, the caller holding the setup lock: `chat.toml` as the one load reads it now decides (a
/// topology that no longer names this computer is `unavailable`), then the workspace's one secrets file, then a fresh
/// `chat.toml` if there was none. The secrets go to `Secrets::file_of`'s name against `chat.toml`'s workspace (the
/// file's default's to `chat-secrets.toml`, any other's to `chat-secrets-<id>.toml`), or to `chat-secrets.toml` when
/// there was no `chat.toml` and this workspace becomes its default; an existing `chat.toml` is left byte for byte. A
/// workspace with no secrets file yet is `too_many_workspaces`, nothing written, when the folder already lists
/// `MAX_WORKSPACES - 1` named files beside the default's: a new name sorting before a served one would push that one past
/// the load's bound. It logs nothing of its own: it runs on the blocking pool, and its reason goes back to the caller
/// (the one load it makes can log its fixed warnings: an ignored mapping, secrets with no user token).
fn save_tokens(env: &Env, workspace: &str, bot: &Secret, user: &Secret, app: &Secret) -> Result<(), Unsaved> {
    let not_saved = |why: String| (ChatCode::NotSaved, Some(why));
    let dir = config::config_dir(env).ok_or_else(|| not_saved("no config folder".into()))?;
    let load = <Chat>::load(env, &config::read_file);
    let (fresh, default) = match load.config {
        None => return Err((ChatCode::Unavailable, None)),
        Some(Ok(c)) => (None, c.workspace().to_string()),
        Some(Err(ConfigError::Missing)) => (Some(config::fresh(workspace).map_err(|e| not_saved(format!("a fresh chat.toml: {e}")))?), workspace.to_string()),
        Some(Err(e)) => return Err(not_saved(format!("a refused chat.toml is left as it is: {e}"))),
    };
    if workspace != default && !load.named.iter().any(|w| w == workspace) && load.named.len() >= MAX_WORKSPACES - 1 {
        return Err((ChatCode::TooManyWorkspaces, None));
    }
    let file = Secrets::file_of(workspace, &default).map_err(|e| not_saved(format!("the secrets' file: {e}")))?;
    let text = config::secrets(workspace, bot, user, app).map_err(|e| not_saved(format!("the secrets' text: {e}")))?;
    let written = |e: std::io::Error| not_saved(format!("a write failed: {:?}", e.kind()));
    config::write_secrets(&dir, &file, &text, Writers::platform()).map_err(written)?;
    if let Some(text) = fresh {
        config::write_config(&dir, &text, Writers::platform()).map_err(written)?;
    }
    Ok(())
}

/// `chat.channels`: the channels the app of the request's workspace (absent: `chat.toml`'s) can see, with whether it is in
/// each; the daemon's own list, whole or an error. A workspace whose listener does not run is `unavailable`.
pub(crate) async fn channels(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, window: bool) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_CHANNELS;
    let listed = async {
        if !window {
            return Err(ChatCode::BadRequest);
        }
        let req = decode::<ChatChannelsReq>(payload)?;
        // The request's workspace, else `chat.toml`'s (C4), fixed whatever runs.
        let workspace = req.workspace.or_else(|| chat.default_workspace()).ok_or(ChatCode::Unavailable)?;
        let api = chat.service_of(&workspace).ok_or(ChatCode::Unavailable)?.api().map_err(|_| ChatCode::Unavailable)?;
        let list = api.channels().await.map_err(|e| code_of(&e))?;
        // An entry whose id the wire refuses is left out, as the call leaves out one with no id.
        let channels = list.channels.into_iter().filter_map(info).collect();
        Ok(ChatChannelsRes { channels, more: list.more })
    };
    match listed.await {
        Ok(res) => reply(req_id, NAME, &res),
        Err(code) => Ok(refuse(req_id, NAME, code)),
    }
}

fn info(c: Channel) -> Option<ChatChannelInfo> {
    Some(ChatChannelInfo { id: ChatChannel::try_from(c.id).ok()?, name: c.name, private: c.private, member: c.member })
}

/// `chat.pair`: the row's folder paired with `channel` of the request's workspace (absent: `chat.toml`'s), or unpaired when
/// there is none. In order: a window's connection; the row, admitted as an action is but with no mapping needed, against
/// that workspace's running service; for a channel, that the workspace's app is in it (joined here when it is public and
/// the app can join; a channel the workspace does not hold is `no_such_channel`); then, under the setup lock, the edit of the file as it stands now and, only when
/// it changed, its write and the reload. The reply comes after the reload's wait. Slack's calls come before the lock, so
/// a slow one never holds up another window's setup.
pub(crate) async fn pair(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces, window: bool) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_PAIR;
    match paired(payload, chat, rows, window).await {
        Ok(()) => reply(req_id, NAME, &ChatPairRes {}),
        Err(code) => Ok(refuse(req_id, NAME, code)),
    }
}

async fn paired(payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces, window: bool) -> Result<(), ChatCode> {
    if !window {
        return Err(ChatCode::BadRequest);
    }
    let req = decode::<ChatPairReq>(payload)?;
    // The request's workspace, else `chat.toml`'s (C4), fixed whatever runs.
    let workspace = req.workspace.clone().or_else(|| chat.default_workspace()).ok_or(ChatCode::Unavailable)?;
    // Admission canonicalizes the row's folder on the shared home, which can block.
    let (held, row, ws, id) = (chat.clone(), rows.clone(), workspace.clone(), req.row.clone());
    let (service, fact) = tokio::task::spawn_blocking(move || held.admit_row(&row, &id, &ws)).await.unwrap_or(Err(ChatCode::Failed))?;
    if let Some(channel) = &req.channel {
        in_channel(&service, channel.as_str()).await?;
    }
    let target = req.channel.as_ref().map(|c| (workspace.clone(), c.as_str().to_string()));
    edit_and_reload(chat, rows, &req.row, fact.canonical.to_string_lossy().into_owned(), target, Some(&workspace), &Asker { who: "window", by: Pairer::Window }).await.map_err(|r| r.code)?;
    Ok(())
}

/// What a pairing's edit found: whether the file changed, and the folder's pairing the file held before it, its channel's
/// name read from the workspace's running cache before the reload that stops it.
struct Edited {
    changed: bool,
    before: Option<ChatPairing>,
    /// The file the edit wrote delivers mail, so a new mapping in it does.
    delivering: bool,
}

/// Who asks a pairing: the name its log line gives (`window`, or `session <handle> (row <id>)`) and what it may undo.
struct Asker<'a> {
    who: &'a str,
    by: Pairer,
}

/// The one pairing core, window's and session's alike: under the setup lock, the edit and write of `chat.toml` ([`save`])
/// and, only when the file changed, one info line naming who asked and the reload.
/// `wait_on` is the workspace the reply waits for; none means the workspace the folder's old mapping named. The caller's
/// checks and Slack calls come before the lock, so a slow one never holds up another's setup.
async fn edit_and_reload(
    chat: &Arc<Chat>,
    rows: &Workspaces,
    row: &str,
    folder: String,
    target: Option<(String, String)>,
    wait_on: Option<&str>,
    asker: &Asker<'_>,
) -> Result<Edited, Refused> {
    let setup = chat.setup().await;
    let (env, to, by) = (chat.env.clone(), target.clone(), asker.by);
    let saved = tokio::task::spawn_blocking(move || save(&env, &folder, to.as_ref().map(|(w, c)| (w.as_str(), c.as_str())), by)).await;
    let (changed, saved) = saved.unwrap_or(Err(ChatCode::Failed.into()))?;
    let (before, delivering) = (pairing_of(chat, &saved.before), saved.delivering);
    if changed {
        let (workspace, channel) = match (&target, &before) {
            (Some((w, c)), _) => (w.as_str(), c.as_str()),
            (None, Some(b)) => (b.workspace.as_str(), b.channel.as_str()),
            (None, None) => ("", ""),
        };
        let outcome = if target.is_some() { "paired" } else { "unpaired" };
        // Every field is a quoted, escaped string: text a client chose (a row as typed) never ends or forges a line.
        tracing::info!(by = asker.who, row, workspace, channel, outcome, "project chat: a pairing changed");
        let on = wait_on.map(str::to_string).or_else(|| before.as_ref().map(|b| b.workspace.clone())).or_else(|| chat.default_workspace());
        chat.reload_for(&setup, rows, on.as_deref().unwrap_or_default()).await;
    }
    Ok(Edited { changed, before, delivering })
}

/// The pairing a resolution holds as the wire tells it, as `chat.resolve` does: the workspace's short name, the channel's name
/// from its running service's cache, and whether mail is delivered.
fn pairing_of(chat: &Arc<Chat>, resolution: &Resolution) -> Option<ChatPairing> {
    let Resolution::Mapped { workspace, channel, .. } = resolution else { return None };
    let name = chat.service_of(workspace).and_then(|s| s.api().ok()).and_then(|api| api.cached_name(channel));
    let delivers = resolution.delivers() && chat.status_of(workspace) == Status::Running;
    Some(ChatPairing { workspace: workspace.clone(), workspace_tag: chat.tag_of(workspace), channel: ChatChannel::try_from(channel.clone()).ok()?, name, delivers })
}

/// A refusal: its code, and the body a reader of that code needs (`already_paired`'s pairing).
struct Refused {
    code: ChatCode,
    body: Option<serde_json::Value>,
}

impl From<ChatCode> for Refused {
    fn from(code: ChatCode) -> Refused {
        Refused { code, body: None }
    }
}

/// `chat.row_pair`: a session pairs the folder of any row of this daemon (named by id or slug) with a channel (given by its
/// id, in the request's workspace, absent `chat.toml`'s), or, with no channel, unpairs it, through the same core
/// as the window's pairing. In order, each refusal before anything is read past it: `by` (its handle bound to its row, as a
/// row's post's is; attribution among one account's programs, not a security boundary), `bad_request`; the target row
/// (`stale_row`); a daemon that is not the hub, or holds no `chat.toml` (`unavailable`); the inert anchor (`unmapped`). An
/// unpairing needs no running service, since it calls no Slack; a pairing needs its workspace's (`unavailable`), and a
/// channel that is not an id is `bad_request` (a name is not looked up yet). A dry run of the edit refuses before any join
/// (`already_paired`, `channel_taken`) or answers `unchanged`, and only then is the channel joined, if need be, and the edit
/// made. Every changed pairing is logged by the core, and every refusal here in one line with who asked and its code, never a
/// path or a channel name.
pub(crate) async fn row_pair(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_ROW_PAIR;
    let req = match decode::<ChatRowPairReq>(payload) {
        Ok(r) => r,
        Err(code) => {
            tracing::info!(?code, "project chat: a session's pairing was not a request");
            return Ok(refuse(req_id, NAME, code));
        }
    };
    let who = format!("session {} (row {})", req.by.name, req.by.row);
    match row_paired(&req, &Asker { who: &who, by: Pairer::Session }, chat, rows).await {
        Ok(res) => reply(req_id, NAME, &res),
        Err(Refused { code, mut body }) => {
            tracing::info!(by = who.as_str(), row = req.row.as_str(), workspace = ?req.workspace, ?code, "project chat: a session's pairing was refused");
            // The pairing the text holds, as the wire tells it, with the workspace's short name and the channel's cached name.
            let held = body.as_ref().filter(|_| code == ChatCode::AlreadyPaired).and_then(|b| Some(Resolution::Mapped { workspace: b["workspace"].as_str()?.into(), channel: b["channel"].as_str()?.into(), mail_after: None }));
            if let (Some(p), Some(b)) = (held.and_then(|h| pairing_of(chat, &h)).and_then(|p| serde_json::to_value(p).ok()), body.as_mut().and_then(|b| b.as_object_mut())) {
                b.extend(p.as_object().cloned().unwrap_or_default());
            }
            match body {
                Some(body) => refuse_with(req_id, NAME, code, &body),
                None => Ok(refuse(req_id, NAME, code)),
            }
        }
    }
}

async fn row_paired(req: &ChatRowPairReq, asker: &Asker<'_>, chat: &Arc<Chat>, rows: &Workspaces) -> Result<ChatRowPairRes, Refused> {
    // The wire refuses an empty handle, which a row with none would otherwise bind.
    if !is_bound(&req.by.name, &req.by.row, rows) {
        return Err(ChatCode::BadRequest.into());
    }
    // A slug names a row, but only its id is the registry's current entry (`current_row`).
    let id = rows.resolve(Some(&req.row)).ok_or(ChatCode::StaleRow)?.workspace_id.clone();
    // No chat configuration is held: not the hub at its boot, or no `chat.toml` (a connect makes one). Nothing is read or written.
    if matches!(chat.config().as_ref(), None | Some(Err(ConfigError::Missing))) {
        return Err(ChatCode::Unavailable.into());
    }
    let (held, at) = (rows.clone(), id.clone());
    let fact = tokio::task::spawn_blocking(move || current_row(&held, &at)).await.unwrap_or(None).ok_or(ChatCode::StaleRow)?;
    if fact.inert {
        return Err(ChatCode::Unmapped.into());
    }
    let folder = fact.canonical.to_string_lossy().into_owned();
    let Some(channel) = req.channel.as_deref() else {
        let edited = edit_and_reload(chat, rows, &id, folder.clone(), None, None, asker).await?;
        let outcome = if edited.changed { ChatPairOutcome::Unpaired } else { ChatPairOutcome::Unchanged };
        return Ok(ChatRowPairRes { outcome, folder, pairing: edited.before.filter(|_| edited.changed) });
    };
    let workspace = req.workspace.clone().or_else(|| chat.default_workspace()).ok_or(ChatCode::Unavailable)?;
    let service = chat.service_of(&workspace).ok_or(ChatCode::Unavailable)?;
    // A channel is given by its id; a name is not looked up yet.
    let Given::Id(channel) = chat_api::given(channel) else { return Err(ChatCode::BadRequest.into()) };
    let (channel, name) = (channel.to_string(), None::<String>);
    // A dry run of the edit before any join, so a pairing that is refused or changes nothing joins nothing.
    let (env, at, target) = (chat.env.clone(), folder.clone(), (workspace.clone(), channel.clone()));
    let dry = tokio::task::spawn_blocking(move || edit(&env, &at, Some((&target.0, &target.1)), Pairer::Session)).await;
    let dry = dry.unwrap_or(Err(ChatCode::Failed.into()))?;
    if dry.edited.is_none() {
        let pairing = pairing_of(chat, &dry.before).map(|p| ChatPairing { name: name.or(p.name), ..p });
        return Ok(ChatRowPairRes { outcome: ChatPairOutcome::Unchanged, folder, pairing });
    }
    let name = in_channel(&service, &channel).await?;
    // Between the join and the lock a window's pairing or a hand edit may land: the edit under the lock is the session's own.
    sot_platform::hold_point_async!("chat.pair.joined", Arc::as_ptr(chat) as usize);
    let target = Some((workspace.clone(), channel.clone()));
    let edited = edit_and_reload(chat, rows, &id, folder.clone(), target, Some(&workspace), asker).await.map_err(joined)?;
    // The reply is what the edit wrote, not a second read: the pairing was written, whatever became of the row since.
    let delivers = edited.delivering && chat.status_of(&workspace) == Status::Running;
    let channel = ChatChannel::try_from(channel).map_err(|_| ChatCode::BadRequest)?;
    let pairing = ChatPairing { workspace_tag: chat.tag_of(&workspace), workspace, channel, name: Some(name), delivers };
    let outcome = if edited.changed { ChatPairOutcome::Paired } else { ChatPairOutcome::Unchanged };
    Ok(ChatRowPairRes { outcome, folder, pairing: Some(pairing) })
}

/// A refusal after the app is in the channel (it joined, or already was): its body says so, since the pairing is unchanged
/// but the app's membership may not be.
fn joined(r: Refused) -> Refused {
    let mut body = r.body.unwrap_or_else(|| serde_json::json!({}));
    body["joined"] = true.into();
    Refused { code: r.code, body: Some(body) }
}

/// That the app is in the channel, from one read of its facts: a member passes, a private channel it is not in is
/// `not_in_channel` unjoined, and a public one is joined once. A join Slack refused is `not_in_channel` (its code is
/// logged: `missing_scope` is an app made before the manifest carried `channels:join`); a join with no usable
/// acknowledgement is settled by reading the channel again, never sent twice. The channel's name, as its facts give it.
async fn in_channel(service: &Arc<Service>, channel: &str) -> Result<String, ChatCode> {
    let api = service.api().map_err(|_| ChatCode::Unavailable)?;
    // Slack's `channel_not_found` to this workspace's app: the channel is another workspace's, or none (PS9).
    let facts = api.channel(channel).await.map_err(|e| match &e {
        ApiError::Slack(code) if code == "channel_not_found" => ChatCode::NoSuchChannel,
        _ => code_of(&e),
    })?;
    if facts.member {
        return Ok(facts.name);
    }
    if facts.private {
        return Err(ChatCode::NotInChannel);
    }
    let joined = match api.join(channel).await.map_err(|e| code_of(&e))? {
        JoinOutcome::Joined => true,
        JoinOutcome::Refused(code) => {
            tracing::warn!(code, "project chat: Slack refused the app's join of a channel");
            false
        }
        JoinOutcome::Unknown => api.channel(channel).await.map_err(|e| code_of(&e))?.member,
    };
    if joined { Ok(facts.name) } else { Err(ChatCode::NotInChannel) }
}

/// The edit and its write, run with the setup lock held: whether the file changed, and what the edit found.
fn save(env: &Env, folder: &str, target: Option<(&str, &str)>, by: Pairer) -> Result<(bool, Edit), Refused> {
    let edit = edit(env, folder, target, by)?;
    if let Some(text) = &edit.edited {
        config::write_config(&edit.dir, text, Writers::platform()).map_err(|e| not_saved(&e.kind()))?;
    }
    Ok((edit.edited.is_some(), edit))
}

fn not_saved(why: &dyn std::fmt::Debug) -> ChatCode {
    tracing::warn!(?why, "project chat: a pairing was not saved");
    ChatCode::NotSaved
}

/// What [`edit`] made: the configuration folder, the text of `chat.toml` after the edit (none when it changes nothing), the
/// folder's mapping in the text before it, and whether the file delivers mail.
struct Edit {
    dir: std::path::PathBuf,
    edited: Option<String>,
    before: Resolution,
    delivering: bool,
}

/// The edit, with no write. The hub fact is asked again first, so a hub deposed since its boot writes nothing in the minute
/// before it steps down. The file is read as the reload reads it (`config::read_file`, not the boot's injected reader,
/// which is the boot's alone), so the text edited is the text the reload will load, a hand edit made since the start
/// included. A text the load refuses is `not_saved` and never rewritten; a pairing that changes nothing is `None`, and a
/// new mapping in a delivering file takes the clock's now for its floor.
fn edit(env: &Env, folder: &str, target: Option<(&str, &str)>, by: Pairer) -> Result<Edit, Refused> {
    if !is_hub(env) {
        return Err(ChatCode::Unavailable.into());
    }
    let dir = config::config_dir(env).ok_or_else(|| not_saved(&"no config folder"))?;
    let text = config::read_file(&dir.join(config::CONFIG_FILE)).map_err(|e| not_saved(&e.kind()))?;
    let loaded = Config::parse(&text, &canon_of);
    let (before, delivering) = (config::resolve(&loaded, folder), loaded.as_ref().is_ok_and(|c| c.mail_after().is_some()));
    match config::pair(&text, folder, target, Some(now()), &canon_of, by) {
        Ok(edited) => Ok(Edit { dir, edited, before, delivering }),
        Err(PairError::ChannelTaken) => Err(ChatCode::ChannelTaken.into()),
        Err(PairError::FolderPaired(workspace, channel)) => Err(Refused { code: ChatCode::AlreadyPaired, body: Some(serde_json::json!({"workspace": workspace, "channel": channel})) }),
        Err(PairError::Config(e)) => Err(not_saved(&e).into()),
    }
}

/// The clock's reading now, as a message timestamp.
fn now() -> Ts {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    Ts::parse(&format!("{}.{:06}", t.as_secs(), t.subsec_micros())).expect("seconds and six digits are a timestamp")
}

/// `chat.workspaces`: `chat.toml`'s workspace and every connected (admitted) workspace, each with its last name, whether
/// its chat runs and, while it runs, the name Slack gave its Slack app's bot (so a window can name the app to keep),
/// ordered by name with the id breaking a tie, at most `MAX_WORKSPACES`. A read of memory that makes no Slack call; a
/// daemon with no admitted `chat.toml` is `unavailable`.
pub(crate) async fn workspaces(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_WORKSPACES;
    if let Err(code) = decode::<ChatWorkspacesReq>(payload) {
        return Ok(refuse(req_id, NAME, code));
    }
    let Some((default, mut workspaces)) = chat.workspaces() else { return Ok(refuse(req_id, NAME, ChatCode::Unavailable)) };
    workspaces.sort_by(|a, b| (a.name.as_deref().unwrap_or(&a.id), &a.id).cmp(&(b.name.as_deref().unwrap_or(&b.id), &b.id)));
    workspaces.truncate(MAX_WORKSPACES);
    reply(req_id, NAME, &ChatWorkspacesRes { default, workspaces })
}
