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

pub(super) use {concept::*, files::*, kernel::*, monitor::*, pages::*, preview::*, repl::*, tree::*, workspace::*};
pub(crate) use {
    concept::ConceptWriteResult,
    files::{DirCreateResult, FileDeleteResult, FileWriteResult},
    kernel::{DefinitionInfo, MarkdownToken, MethodInfo, ModuleInfo, ScanEntity, ScanModule, ScanType},
    repl::ReplRunFileInfo,
    tree::DirEntry,
    workspace::{AccountInfo, WorkspaceCreatedInfo, WorkspaceDestroyedInfo, WorkspaceInfo},
};
