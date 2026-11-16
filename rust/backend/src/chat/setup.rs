//! The window's setup ops that need a running chat: `chat.channels`, the picker's list, and `chat.pair`, which maps a
//! row's folder to a channel or unmaps it, rewrites `chat.toml` and reloads. Both are jobs, taken from a window's
//! connection only (a row's agent has no use for them), and neither makes a Slack call without a running service. The
//! edit of the file and the reload that follows it hold the setup lock together, so two windows' pairings and a connect
//! never lose one another's change.

use std::sync::Arc;

use anyhow::Result;
use sot_chat::api::{Channel, JoinOutcome};
use sot_chat::config::{self, PairError, Writers};
use sot_chat::service::Service;
use sot_platform::host::env::Env;
use sot_protocol::chat::Ts;
use sot_protocol::ops::{op, ChatChannel, ChatChannelInfo, ChatChannelsReq, ChatChannelsRes, ChatCode, ChatPairReq, ChatPairRes};
use sot_protocol::reply::HandlerOutput;

use super::ops::{code_of, decode, refuse, reply};
use super::{canon_of, is_hub, Chat};
use crate::rows::Workspaces;

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
