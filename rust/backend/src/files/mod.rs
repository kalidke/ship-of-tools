//! A workspace's files, served to clients: the Files tree, editor file IO,
//! the `.concept/` store and the change watcher.

pub(super) mod concept;
pub(super) mod concept_ops;
pub(crate) mod confine;
pub(super) mod io;
pub(super) mod io_ops;
pub(super) mod preview;
pub(super) mod transfer;
pub(super) mod tree;
pub(super) mod tree_ops;
pub(super) mod watcher;
