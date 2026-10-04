//! Files mode's file operations: the new-file and delete prompts, a driven reveal, file paths, transfers and listing refreshes.

use super::*;

pub(in crate::ui) mod keys;
mod listing;
mod paths;
mod prompt;
mod replies;
mod reveal;
mod transfer;

pub(in crate::ui) use paths::*;
pub(in crate::ui) use prompt::*;
pub(in crate::ui) use transfer::*;

pub(crate) mod download;
