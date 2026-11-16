//! The window's setup ops: `chat.connect`, which checks three tokens with Slack, saves them and reloads the chat from its
//! files with no daemon restart; `chat.channels`, the picker's list; and `chat.pair`, which maps a row's folder to a
//! channel or unmaps it, rewrites `chat.toml` and reloads. All three are jobs, taken from a window's connection only (a
//! row's agent has no use for them, and no token goes on a command line). The edit of a file and the reload that follows
//! it hold the setup lock together, so two windows' pairings and a connect never lose one another's change. A token is a
//! `Secret` from its decode on: no log line, reply or error here carries a byte of one, and no request payload is
//! logged.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use sot_chat::api::{ApiError, Channel, Identified, JoinOutcome, TOKEN_REFUSALS};
use sot_chat::config::{self, ConfigError, PairError, Writers};
use sot_chat::http::{Http, Secret};
use sot_chat::service::Service;
use sot_chat::socket::SocketError;
use sot_platform::host::env::Env;
use sot_protocol::chat::Ts;
use sot_protocol::ops::{
    op, ChatChannel, ChatChannelInfo, ChatChannelsReq, ChatChannelsRes, ChatCode, ChatConnectReq, ChatConnectRes, ChatPairReq,
    ChatPairRes, ChatUnconfigured,
};
use sot_protocol::reply::HandlerOutput;

use super::ops::{code_of, decode, refuse, reply};
use super::{canon_of, is_hub, Chat, Status};
use crate::rows::Workspaces;

type Checked<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

/// The connect's Slack checks: the bot token's identity with no workspace expected (and the user token's, by the same
/// `identify`), then the app token's one `apps.connections.open`. The product's reach Slack ([`Verifier::product`]); a test's reach the fake.
pub(crate) struct Verifier {
    pub(crate) identify: Box<dyn Fn(Secret) -> Checked<Identified, ApiError> + Send + Sync>,
    pub(crate) app: Box<dyn Fn(Secret) -> Checked<(), SocketError> + Send + Sync>,
}

impl Verifier {
    /// `api::identify` and `socket::check_app_token` through one client, made at the first check, so every connect's
    /// calls share its bound on operations at once (`http::MAX_OPERATIONS`).
    pub(crate) fn product() -> Verifier {
        let client = Arc::new(std::sync::OnceLock::new());
        let one = move || client.get_or_init(Http::new).clone();
        let other = one.clone();
        Verifier {
            identify: Box::new(move |bot| {
                let http = one();
                Box::pin(async move { sot_chat::api::identify(&http.map_err(ApiError::Http)?, bot).await })
            }),
            app: Box::new(move |app| {
                let http = other();
                Box::pin(async move { sot_chat::socket::check_app_token(&http.map_err(SocketError::Http)?, app).await })
            }),
        }
    }

    /// Both checks against the fake Slack's base.
    #[cfg(test)]
    pub(crate) fn at(fake: &sot_chat::fake_slack::FakeSlack) -> Verifier {
        let (http, base) = (fake.http(), fake.url("slack.com", "/api/"));
        let (h, b) = (http.clone(), base.clone());
        Verifier {
            identify: Box::new(move |bot| {
                let (http, base) = (h.clone(), b.clone());
                Box::pin(async move { sot_chat::api::identify_for_test(&http, bot, base).await })
            }),
            app: Box::new(move |app| {
                let (http, base) = (http.clone(), base.clone());
                Box::pin(async move { sot_chat::socket::check_app_token_for_test(&http, app, &base).await })
            }),
        }
    }

    /// Checks that never reach Slack, for a chat no test connects.
    #[cfg(test)]
    pub(crate) fn none() -> Verifier {
        Verifier { identify: Box::new(|_| Box::pin(async { Err(ApiError::Closed) })), app: Box::new(|_| Box::pin(async { Err(SocketError::BadRequest) })) }
    }
}

/// Why the hub's chat is off when a connect fixes it: no `chat.toml` or no secrets, secrets the load refused, a start
/// that failed, or a running chat whose service holds no user token (`no_user_token`). A refused
/// `chat.toml` is not one (a connect never rewrites it), nor is any other status.
pub(crate) fn unconfigured(status: Status, no_user_token: bool) -> Option<ChatUnconfigured> {
    match status {
        Status::Running if no_user_token => Some(ChatUnconfigured::NoUserToken),
        Status::Refused(ConfigError::Missing) | Status::Secrets(ConfigError::Missing) => Some(ChatUnconfigured::NotConnected),
        Status::Secrets(_) => Some(ChatUnconfigured::TokensRefused),
        Status::Failed(_) => Some(ChatUnconfigured::StartFailed),
        _ => None,
    }
}

/// `chat.connect`: three tokens checked with Slack, saved, and the chat reloaded from its files. The order and its codes
/// are `rust/chat/CLAUDE.md` guarantee 31's. Nothing here logs on the blocking pool: the writes answer their reason as a
/// value, and this task logs it.
pub(crate) async fn connect(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces, window: bool) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_CONNECT;
    let req = match (window, serde_json::from_value::<ChatConnectReq>(payload)) {
        (true, Ok(r)) => r,
        _ => return Ok(refuse(req_id, NAME, ChatCode::BadRequest)),
    };
    if chat.shared.closed_now() {
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
    drop(req);
    // Only Slack's answers that name the token refuse it; a rate limit, an outage or a transport failure is `failed`.
    let workspace = match (chat.verify.identify)(bot.clone()).await {
        Ok(id) => id.workspace,
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
    match (chat.verify.app)(app.clone()).await {
        Ok(()) => {}
        Err(SocketError::Rejected) => return Ok(refuse(req_id, NAME, ChatCode::AppTokenRefused)),
        Err(e) => {
            tracing::warn!(code = e.code(), "project chat: a connect could not check the app-level token");
            return Ok(refuse(req_id, NAME, ChatCode::Failed));
        }
    }
    let held = chat.setup().await;
    // A retirement that began while Slack was asked: nothing is written for a daemon that is going away.
    if chat.shared.closed_now() {
        return Ok(refuse(req_id, NAME, ChatCode::Unavailable));
    }
    let (env, named) = (chat.env.clone(), workspace.clone());
    let saved = tokio::task::spawn_blocking(move || save_tokens(&env, &named, &bot, &user, &app)).await;
    if let Err((code, why)) = saved.unwrap_or(Err((ChatCode::NotSaved, Some("the write's task ended".into())))) {
        if let Some(why) = why {
            tracing::warn!(%why, "project chat: a connect saved nothing");
        }
        return Ok(refuse(req_id, NAME, code));
    }
    let status = chat.reload(&held, rows).await;
    tracing::info!(workspace, %status, "project chat: a window's connect saved the tokens and reloaded");
    reply(req_id, NAME, &ChatConnectRes { workspace })
}

/// Why the connect's writes did not happen: the reply's code and, for the caller to log, a fixed reason with the load's
/// refusal or the error's kind, never a value of the files.
type Unsaved = (ChatCode, Option<String>);

/// The connect's writes, the caller holding the setup lock: `chat.toml` as the one load reads it now decides (a
/// topology that no longer names this computer is `unavailable`), then the secrets, then a fresh `chat.toml` if there
/// was none. It logs nothing: it runs on the blocking pool, and its reason goes back to the caller.
fn save_tokens(env: &Env, workspace: &str, bot: &Secret, user: &Secret, app: &Secret) -> Result<(), Unsaved> {
    let not_saved = |why: String| (ChatCode::NotSaved, Some(why));
    let dir = config::config_dir(env).ok_or_else(|| not_saved("no config folder".into()))?;
    let fresh = match <Chat>::load(env, &config::read_file).config {
        None => return Err((ChatCode::Unavailable, None)),
        Some(Ok(c)) if c.workspace() != workspace => return Err((ChatCode::OtherWorkspace, None)),
        Some(Ok(_)) => None,
        Some(Err(ConfigError::Missing)) => Some(config::fresh(workspace).map_err(|e| not_saved(format!("a fresh chat.toml: {e}")))?),
        Some(Err(e)) => return Err(not_saved(format!("a refused chat.toml is left as it is: {e}"))),
    };
    let text = config::secrets(workspace, bot, user, app).map_err(|e| not_saved(format!("the secrets' text: {e}")))?;
    let written = |e: std::io::Error| not_saved(format!("a write failed: {:?}", e.kind()));
    config::write_secrets(&dir, &text, Writers::platform()).map_err(written)?;
    if let Some(text) = fresh {
        config::write_config(&dir, &text, Writers::platform()).map_err(written)?;
    }
    Ok(())
}

/// `chat.channels`: the channels the app can see, with whether it is in each; the daemon's own list, whole or an error.
pub(crate) async fn channels(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, window: bool) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_CHANNELS;
    let listed = async {
        if !window {
            return Err(ChatCode::BadRequest);
        }
        decode::<ChatChannelsReq>(payload)?;
        let api = chat.service().ok_or(ChatCode::Unavailable)?.api().map_err(|_| ChatCode::Unavailable)?;
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

/// `chat.pair`: the row's folder paired with `channel`, or unpaired when there is none. In order: a window's connection;
/// the row, admitted as an action is but with no mapping needed; for a channel, that the app is in it (joined here when
/// it is public and the app can join); then, under the setup lock, the edit of the file as it stands now and, only when
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
    // Admission canonicalizes the row's folder on the shared home, which can block.
    let (held, row) = (chat.clone(), rows.clone());
    let (service, fact) = tokio::task::spawn_blocking(move || held.admit_row(&row, &req.row)).await.unwrap_or(Err(ChatCode::Failed))?;
    if let Some(channel) = &req.channel {
        in_channel(&service, channel.as_str()).await?;
    }
    let setup = chat.setup().await;
    let (env, folder, channel) = (chat.env.clone(), fact.canonical.to_string_lossy().into_owned(), req.channel);
    let changed = tokio::task::spawn_blocking(move || save(&env, &folder, channel.as_ref().map(ChatChannel::as_str))).await.unwrap_or(Err(ChatCode::Failed))?;
    if changed {
        chat.reload(&setup, rows).await;
    }
    Ok(())
}

/// That the app is in the channel, from one read of its facts: a member passes, a private channel it is not in is
/// `not_in_channel` unjoined, and a public one is joined once. A join Slack refused is `not_in_channel` (its code is
/// logged: `missing_scope` is an app made before the manifest carried `channels:join`); a join with no usable
/// acknowledgement is settled by reading the channel again, never sent twice.
async fn in_channel(service: &Arc<Service>, channel: &str) -> Result<(), ChatCode> {
    let api = service.api().map_err(|_| ChatCode::Unavailable)?;
    let facts = api.channel(channel).await.map_err(|e| code_of(&e))?;
    if facts.member {
        return Ok(());
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
    if joined { Ok(()) } else { Err(ChatCode::NotInChannel) }
}

/// The edit and its write, run with the setup lock held. The hub fact is asked again first, so a hub deposed since its
/// boot writes nothing in the minute before it steps down. The file is read as the reload reads it (`config::read_file`,
/// not the boot's injected reader, which is the boot's alone), so the text edited is the text the reload will load, a hand
/// edit made since the start included. A text the load refuses is `not_saved` and never rewritten; a pairing that
/// changes nothing writes nothing (`false`), and a new mapping in a delivering file takes the clock's now for its floor.
fn save(env: &Env, folder: &str, channel: Option<&str>) -> Result<bool, ChatCode> {
    if !is_hub(env) {
        return Err(ChatCode::Unavailable);
    }
    let not_saved = |why: &dyn std::fmt::Debug| {
        tracing::warn!(?why, "project chat: a pairing was not saved");
        ChatCode::NotSaved
    };
    let dir = config::config_dir(env).ok_or_else(|| not_saved(&"no config folder"))?;
    let text = config::read_file(&dir.join(config::CONFIG_FILE)).map_err(|e| not_saved(&e.kind()))?;
    match config::pair(&text, folder, channel, Some(now()), &canon_of) {
        Ok(None) => Ok(false),
        Ok(Some(edited)) => {
            config::write_config(&dir, &edited, Writers::platform()).map_err(|e| not_saved(&e.kind()))?;
            Ok(true)
        }
        Err(PairError::ChannelTaken) => Err(ChatCode::ChannelTaken),
        Err(PairError::Config(e)) => Err(not_saved(&e)),
    }
}

/// The clock's reading now, as a message timestamp.
fn now() -> Ts {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    Ts::parse(&format!("{}.{:06}", t.as_secs(), t.subsec_micros())).expect("seconds and six digits are a timestamp")
}
