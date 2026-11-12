//! The daemon's half of channel mail, which the chat's owner runs on the blocking pool: who a channel's mail goes to, the
//! one guarded filing, the write of a boundary, and the start's read of every inbox. Each answers what `Delivery` asks
//! for and decides nothing of when.

#![allow(dead_code, reason = "the chat's owner composes these in the next landing")]

use std::collections::BTreeMap;
use std::path::Path;

use sot_chat::delivery::FILE_GAP;
use sot_chat::mail::{body, filed_of, Filed, SENDER};
use sot_chat::state::{self, StateError};
use sot_platform::host::env::Env;
use sot_protocol::chat::{ChatRecord, Ts};
use sot_protocol::CommFileReq;

use crate::comm::mail::filer::{file_comm, heartbeat_fresh};
use crate::comm::registry::lock::POLL;
use crate::comm::registry::registry::{entry_row, read_registry_fresh, unix_now_secs};
use crate::rows::handle::bound_handle;
use crate::rows::Workspaces;

// A registry writer that waits its bound meets free polls after every filing's hold, whatever the hold took.
const _: () = assert!(FILE_GAP.as_millis() >= 4 * POLL.as_millis());

/// The row a folder's mail goes to: its handle, and its id, which the filing names as the row it expects.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Recipient {
    pub(crate) handle: String,
    pub(crate) row: String,
}

/// The folder's one Claude row, else its one Codex row, that has a handle; never the inert anchor, which runs no agent.
/// Absent, with a fixed code, when there is none (`no_row`), two of the kind (`ambiguous_row`), or the handle's registry
/// entry, read fresh, does not name this row with a live session (`no_live_session`). Blocking: it reads the rows'
/// folders and the registry.
pub(crate) fn recipient(rows: &Workspaces, folder: &str, env: &Env, now_secs: u64) -> Result<Recipient, &'static str> {
    let all = rows.list();
    for kind in ["claude", "codex"] {
        let mut of = all
            .iter()
            .filter(|ws| ws.agent() == kind && super::canonical(&ws.project_root).is_some_and(|c| c.to_string_lossy() == folder))
            .filter_map(|ws| Some((ws, bound_handle(ws, &all))).filter(|(_, h)| !h.is_empty()));
        let Some((ws, handle)) = of.next() else { continue };
        if of.next().is_some() {
            return Err("ambiguous_row");
        }
        return live(env, &handle, &ws.workspace_id, now_secs).then(|| Recipient { handle, row: ws.workspace_id.clone() }).ok_or("no_live_session");
    }
    Err("no_row")
}

/// Whether the registry lists `handle` with a host, bound to `row`, and a fresh heartbeat: what the filing will check
/// again under the registry lock. A registry that cannot be read lists no one.
fn live(env: &Env, handle: &str, row: &str, now_secs: u64) -> bool {
    let doc = crate::comm::home::comm_home(env)
        .and_then(|home| read_registry_fresh(&home.join("registry.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let entry = doc.as_ref().map_or(&serde_json::Value::Null, |d| &d["agents"][handle]);
    entry["host"].as_str().is_some_and(|h| !h.is_empty()) && entry_row(entry) == Some(row) && heartbeat_fresh(entry["last_seen"].as_str(), now_secs)
}

/// One filing: nothing at all unless the topology still names this computer (or no one) the hub, then the recipient, then
/// the guarded append, which rechecks the binding and the liveness under the registry lock. `Ok` is `Verdict::Filed`; an
/// `Err` is `Verdict::Refused` with the code the owner logs (`other_hub`, `recipient`'s codes, or the filing's own).
pub(crate) async fn file(env: &Env, rows: &Workspaces, folder: &str, record: &ChatRecord) -> Result<(), String> {
    let (e, r, f) = (env.clone(), rows.clone(), folder.to_string());
    let to = tokio::task::spawn_blocking(move || if super::names_another_hub(&e) { Err("other_hub") } else { recipient(&r, &f, &e, unix_now_secs()) })
        .await
        .map_err(|_| "file_failed")??;
    append(env, &to, record).await
}

/// The guarded append of `record`'s mail to `to`, from `#chat`: it names the row it expects, so a handle moved since the
/// recipient was read files nothing.
pub(crate) async fn append(env: &Env, to: &Recipient, record: &ChatRecord) -> Result<(), String> {
    let req = CommFileReq { from: SENDER.into(), to: to.handle.clone(), text: body(record), broadcast: false, forwarded: false, expected_row: Some(to.row.clone()) };
    match file_comm(env.clone(), req).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err((code, _))) => Err(code),
        Err(_) => Err("file_failed".into()),
    }
}

/// The commit: `boundary` written durably to the channel's file in `dir`, which is made private first. A failed write
/// leaves the old file, which only costs the next start reads; the owner logs the code and goes on. Blocking.
pub(crate) fn commit(dir: &Path, workspace: &str, channel: &str, boundary: &Ts) -> Result<(), StateError> {
    sot_platform::host::paths::ensure_private_dir(dir).map_err(|_| StateError::Unwritable)?;
    state::write(dir, workspace, channel, boundary)
}

/// The start's read: every inbox's `#chat` lines (not only the mapped rows' current ones, since a handle can change
/// while its earlier filings stay in the old inbox), as the filings of `workspace`'s `channels`, one entry for each,
/// empty when it has none. A read that fails in any way is the failure, never no filings: the owner tries again, and
/// nothing is filed until it succeeds. Blocking, up to a held inbox lock's bound.
pub(crate) fn read_filings(env: &Env, workspace: &str, channels: &[&str]) -> Result<BTreeMap<String, Vec<Filed>>, String> {
    let home = crate::comm::home::comm_home(env).ok_or("the comm folder cannot be named")?;
    let mut by: BTreeMap<String, Vec<Filed>> = channels.iter().map(|c| (c.to_string(), Vec::new())).collect();
    for f in crate::comm::mail::inbox::lines_from(&home, SENDER, env)?.iter().filter_map(|m| filed_of(m)) {
        if let Some(v) = by.get_mut(&f.channel).filter(|_| f.workspace == workspace) {
            v.push(f);
        }
    }
    Ok(by)
}

#[cfg(test)]
#[path = "mail_tests.rs"]
mod tests;
