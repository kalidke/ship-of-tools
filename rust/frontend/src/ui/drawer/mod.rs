//! The bottom drawer: its three tenants, the monitor view, the terminal and the REPL.

pub(in crate::ui) mod keys;
pub(in crate::ui) mod chat;
pub(crate) mod monitor;
mod monitor_drive;
pub(in crate::ui) mod repl;
pub(crate) mod terminal;
