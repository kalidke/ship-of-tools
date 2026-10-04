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
