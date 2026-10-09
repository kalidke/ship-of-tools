//! Launching a row's supervisor: the state root, the detached spawn per OS and the Linux row scope.

pub(crate) mod detach;
/// The capsule-only birth parent (Unix): a supervisor is forked after the row's fence is claimed.
#[cfg(unix)]
pub(crate) mod durable;
pub(crate) mod state_root;

/// A4b: a row's own systemd scope is the kill domain of everything the row
/// started, including a descendant that left the agent's process group
/// (`setsid`), which the leg's `killpg` cannot reach. The scope's unit name
/// is the record: `sot-row-<state_dir_hash>-<uuid>.scope`, held by systemd.
#[cfg(target_os = "linux")]
pub(crate) mod row_scope;
#[cfg(target_os = "linux")]
pub(crate) mod row_scope_aim;

use state_root::{qualified_state_root, state_root_inside_project, STATE_ROOT_HINT};
