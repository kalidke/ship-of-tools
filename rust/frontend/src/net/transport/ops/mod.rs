//! The window's daemon ops, one file per family.

use super::*;

mod chat;
mod concept;
mod files;
mod kernel;
mod monitor;
mod pages;
mod preview;
mod repl;
mod selection;
mod tree;
mod workspace;

/// An error reply's words: `<error> [<code>]`, or `<error>` when it names no code; `None` for a reply with no `error`.
fn error_text(payload: &serde_json::Value) -> Option<String> {
    let error = payload.get("error").and_then(|v| v.as_str())?;
    Some(match payload.get("code").and_then(|v| v.as_str()) {
        Some(code) => format!("{error} [{code}]"),
        None => error.to_string(),
    })
}

/// The outcome word of a request an older daemon does not know (`PageReplied`'s `outcome`; a future op's reply carries it
/// the same way).
pub(crate) const UNSUPPORTED: &str = "unsupported";

/// Whether `payload` is the daemon's generic unknown-op answer to `op`: exactly `{"error": "unknown op: <op>"}`, no code
/// and no other field (server/dispatch.rs's catch-all). Names no op, so every op's reply handler may ask. Any other
/// reply, a success that does not parse included, is not this.
fn is_unsupported(op: &str, payload: &serde_json::Value) -> bool {
    payload.as_object().is_some_and(|o| o.len() == 1)
        && payload.get("error").and_then(|v| v.as_str()).is_some_and(|e| e.strip_prefix("unknown op: ") == Some(op))
}

pub(super) use {chat::*, concept::*, files::*, kernel::*, monitor::*, pages::*, preview::*, repl::*, selection::*, tree::*, workspace::*};
pub(crate) use {
    chat::{ChatEvt, ChatReply},
    concept::ConceptWriteResult,
    files::{DirCreateResult, FileDeleteResult, FileWriteResult},
    kernel::{DefinitionInfo, MarkdownToken, MethodInfo, ScanModule, ScanType},
    repl::ReplRunFileInfo,
    selection::SelectionReply,
    tree::DirEntry,
    workspace::{AccountInfo, WorkspaceCreatedInfo, WorkspaceDestroyedInfo, WorkspaceInfo},
};
