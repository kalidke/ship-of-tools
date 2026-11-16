//! The window's setup ops: `chat.connect`, which checks two tokens with Slack, saves them and reloads the chat from its
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
use sot_chat::api::{ApiError, Channel, Identified, JoinOutcome};
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

/// The connect's two Slack checks: the bot token's identity with no workspace expected, then the app token's one
/// `apps.connections.open`. The product's reach Slack ([`Verifier::product`]); a test's reach the fake.
pub(crate) struct Verifier {
    pub(crate) identify: Box<dyn Fn(Secret) -> Checked<Identified, ApiError> + Send + Sync>,
    pub(crate) app: Box<dyn Fn(Secret) -> Checked<(), SocketError> + Send + Sync>,
}

impl Verifier {
    /// `api::identify` and `socket::check_app_token`, each through a client of its own.
    pub(crate) fn product() -> Verifier {
        Verifier {
            identify: Box::new(|bot| Box::pin(async move { sot_chat::api::identify(&Http::new().map_err(ApiError::Http)?, bot).await })),
            app: Box::new(|app| Box::pin(async move { sot_chat::socket::check_app_token(&Http::new().map_err(SocketError::Http)?, app).await })),
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
/// that failed. A refused `chat.toml` is not one (a connect never rewrites it), nor is any other status.
pub(crate) fn unconfigured(status: Status) -> Option<ChatUnconfigured> {
    match status {
        Status::Refused(ConfigError::Missing) | Status::Secrets(ConfigError::Missing) => Some(ChatUnconfigured::NotConnected),
        Status::Secrets(_) => Some(ChatUnconfigured::TokensRefused),
        Status::Failed(_) => Some(ChatUnconfigured::StartFailed),
        _ => None,
    }
}

/// `chat.connect`, in order: a window's connection only (`bad_request`); this daemon booted as the hub and the
/// topology still names it (`unavailable`); each token in the load's grammar; the bot token's identity
/// (`bot_token_refused`, a transport failure `failed`); the app token's open (`app_token_refused`, likewise). Then, under
/// the setup lock, `chat.toml` read as it is now: another workspace's is `other_workspace`, one the load refuses is
/// `not_saved`, a missing one is written fresh after the secrets; the secrets through the private write; then the
/// reload, and the reply `{workspace}` once the reload's wait ends. Nothing is written before both tokens pass.
pub(crate) async fn connect(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces, window: bool) -> Result<HandlerOutput> {
    let req = match (window, serde_json::from_value::<ChatConnectReq>(payload)) {
        (true, Ok(r)) => r,
        _ => return Ok(refuse(req_id, op::CHAT_CONNECT, ChatCode::BadRequest)),
    };
    if chat.shared.closed_now() {
        return Ok(refuse(req_id, op::CHAT_CONNECT, ChatCode::Unavailable));
    }
    let env = chat.env.clone();
    if !tokio::task::spawn_blocking(move || is_hub(&env)).await.unwrap_or(false) {
        return Ok(refuse(req_id, op::CHAT_CONNECT, ChatCode::Unavailable));
    }
    let Ok(bot) = config::bot_token(req.bot_token.expose()) else { return Ok(refuse(req_id, op::CHAT_CONNECT, ChatCode::BotTokenRefused)) };
    let Ok(app) = config::app_token(req.app_token.expose()) else { return Ok(refuse(req_id, op::CHAT_CONNECT, ChatCode::AppTokenRefused)) };
    drop(req);
    let workspace = match (chat.verify.identify)(bot.clone()).await {
        Ok(id) => id.workspace,
        Err(ApiError::Slack(_)) => return Ok(refuse(req_id, op::CHAT_CONNECT, ChatCode::BotTokenRefused)),
        Err(e) => {
            tracing::warn!(code = e.code(), "project chat: a connect could not check the bot token");
            return Ok(refuse(req_id, op::CHAT_CONNECT, ChatCode::Failed));
        }
    };
    match (chat.verify.app)(app.clone()).await {
        Ok(()) => {}
        Err(SocketError::Rejected) => return Ok(refuse(req_id, op::CHAT_CONNECT, ChatCode::AppTokenRefused)),
        Err(e) => {
            tracing::warn!(code = e.code(), "project chat: a connect could not check the app-level token");
            return Ok(refuse(req_id, op::CHAT_CONNECT, ChatCode::Failed));
        }
    }
    let held = chat.setup().await;
    let env = chat.env.clone();
    let named = workspace.clone();
    let saved = tokio::task::spawn_blocking(move || save_tokens(&env, &named, &bot, &app)).await.unwrap_or(Err(ChatCode::NotSaved));
    if let Err(code) = saved {
        return Ok(refuse(req_id, op::CHAT_CONNECT, code));
    }
    let status = chat.reload(&held, rows).await;
    tracing::info!(workspace, %status, "project chat: connected from a window");
    reply(req_id, op::CHAT_CONNECT, &ChatConnectRes { workspace })
}

/// The connect's writes, the caller holding the setup lock: `chat.toml` as the one load reads it now decides (a
/// topology that no longer names this computer is `unavailable`), then the secrets, then a fresh `chat.toml` if there
/// was none. Each refusal and failed write is logged by its code or error kind alone.
fn save_tokens(env: &Env, workspace: &str, bot: &Secret, app: &Secret) -> Result<(), ChatCode> {
    let Some(dir) = config::config_dir(env) else {
        tracing::warn!("project chat: a connect found no config folder");
        return Err(ChatCode::NotSaved);
    };
    let fresh = match <Chat>::load(env, &config::read_file).config {
        None => return Err(ChatCode::Unavailable),
        Some(Ok(c)) if c.workspace() != workspace => return Err(ChatCode::OtherWorkspace),
        Some(Ok(_)) => None,
        Some(Err(ConfigError::Missing)) => Some(config::fresh(workspace).map_err(|_| ChatCode::NotSaved)?),
        Some(Err(e)) => {
            tracing::warn!(refusal = %e, "project chat: a connect leaves a refused chat.toml as it is");
            return Err(ChatCode::NotSaved);
        }
    };
    let written = config::secrets(workspace, bot, app)
        .map_err(|_| None)
        .and_then(|text| config::write_secrets(&dir, &text, Writers::platform()).map_err(|e| Some(e.kind())))
        .and_then(|()| fresh.map_or(Ok(()), |text| config::write_config(&dir, &text, Writers::platform()).map_err(|e| Some(e.kind()))));
    written.map_err(|kind| {
        tracing::warn!(?kind, "project chat: a connect could not write the chat's files");
        ChatCode::NotSaved
    })
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
