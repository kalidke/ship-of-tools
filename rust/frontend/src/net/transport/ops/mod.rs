//! The window's daemon ops, one file per family.

use super::*;

mod concept;
mod files;
mod kernel;
mod monitor;
mod pages;
mod preview;
mod repl;
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

pub(super) use {concept::*, files::*, kernel::*, monitor::*, pages::*, preview::*, repl::*, tree::*, workspace::*};
pub(crate) use {
    concept::ConceptWriteResult,
    files::{DirCreateResult, FileDeleteResult, FileWriteResult},
    kernel::{DefinitionInfo, MarkdownToken, MethodInfo, ScanModule, ScanType},
    repl::ReplRunFileInfo,
    tree::DirEntry,
    workspace::{AccountInfo, WorkspaceCreatedInfo, WorkspaceDestroyedInfo, WorkspaceInfo},
};
