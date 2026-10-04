// workspaces.rs — daemon-side registry of project workspaces.
//
// Per ADR 0014, one Ship of Tools daemon hosts many workspaces. Each workspace
// is a (id, slug, label, project_root) tuple plus references to the
// long-lived per-workspace state the daemon owns (kernel child, file
// watcher, BL tmux session — not all wired through here yet).
//
// This module owns the registry + the on-disk persistence layer; per-
// workspace kernel spawn (task #17) and protocol routing (task #18)
// build on top.
//
// On-disk layout:
//
//   ~/.config/sot/workspaces/<slug>.toml      ← ADR 0014, canonical
//   ~/.config/sot/sessions/<slug>.toml        ← ADR 0013, legacy; read for migration
//
// Read is fail-soft: a missing or malformed file is treated as "no
// workspace by that slug" and we keep going. The daemon always has at
// least the *default* workspace (the one whose project_root matches
// `--project-root`), constructed at startup whether or not a toml
// exists for it.

pub(crate) use crate::rows::anchor::default_row_launch_seed;
pub use crate::rows::gate::StartPermit;
pub use crate::rows::workspace::Phase;
pub(crate) use crate::rows::workspace::{Observation, SupervisorIdentity};
pub use crate::rows::{Workspace, WorkspaceChanged, Workspaces};
pub(crate) use crate::comm::mail::bus::{AgentMessage, AgentReceipt};
pub use crate::rows::store::{legacy_toml_path_for, save, scan_disk, toml_path_for};
pub(crate) use crate::rows::store::{app_config_dir, check_config_dir, declared_host};
