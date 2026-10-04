//! The row ops clients call: workspace.create, destroy, list and activate, and pty.input and pty.screen.

pub(crate) mod create;
pub(crate) mod destroy;
pub(crate) mod list;
pub(crate) mod pty;
