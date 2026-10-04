// handlers.rs — op dispatch for the M1 spike.
//
// Each handler takes a parsed Frame (the codec already verified envelope +
// blob), returns a (Frame, Option<Vec<u8>>) tuple the connection task writes
// back. Handlers borrow the Session for state mutations.
//
// All content here is hardcoded for the spike. The eventual kernel-driven
// path replaces these stubs with calls into ShipToolsKernel over its own pipe;
// the on-the-wire Frame shape stays the same.

pub(crate) use crate::server::reply::HandlerOutput;

pub(crate) use crate::files::preview::{crop::handle_image_crop, handle_preview_get, scale::handle_preview_set_scale};

pub(crate) use crate::server::hello::handle_hello;

pub(crate) use crate::clients::{handle_fe_command_send, handle_fe_presence, handle_fe_sessions, handle_version_query};

pub(crate) use crate::files::tree_ops::{handle_directory_list, handle_nav_toggle_hidden, handle_tree_children, handle_tree_root};

pub(crate) use crate::files::concept_ops::{handle_concept_list, handle_concept_read, handle_concept_write};

pub(crate) use crate::files::io_ops::{handle_dir_create, handle_file_delete, handle_file_read, handle_file_write};

pub(crate) use crate::sidecars::repl::ops::{handle_repl_eval, handle_repl_interrupt, handle_repl_run_file};

pub(crate) use crate::sidecars::repl::execute::handle_repl_execute;

pub(crate) use crate::sidecars::ops::{handle_kernel_request, handle_math_render, handle_pluto_open};

pub(crate) use crate::rows::ops::create::handle_workspace_create;

pub(crate) use crate::files::confine::{canonicalize_and_workspace_root, canonicalize_within_any_workspace};

pub(crate) use crate::pages::ops::{handle_docs_open, handle_quarto_open, handle_video_open};

pub(crate) use crate::paths::valid_name;

pub(crate) use crate::files::transfer::{handle_file_upload, stream_file_download};

pub(crate) use crate::rows::ops::pty::{handle_pty_input, handle_pty_screen};

pub(crate) use crate::rows::run::end::{capsule_destroy_outcome_of, capsule_end_not_reached_payload, default_row_end_response, destroy_capsule_workspace, remove_row_files, CapsuleDestroyOutcome, ALREADY_REMOVED};

pub(crate) use crate::rows::anchor::end_default_row_run;

pub(crate) use crate::rows::ops::destroy::handle_workspace_destroy;

pub(crate) use crate::comm::mail::relay::{handle_agent_filed, handle_agent_send};

pub(crate) use crate::comm::registry::join::handle_agent_join;

pub(crate) use crate::comm::mail::filer::{comm_self_host, comm_topology_hub, file_comm, handle_comm_file};

pub(crate) use crate::server::conn::handle_ping;

pub(crate) use crate::comm::registry::registry::{clear_comm_unread, comm_handle_for_workspace, comm_registry_path, host_matches, iso8601_utc_from_secs, iso8601_utc_now, read_comm_agents, read_registry_fresh, remove_comm_agents_for_workspace, unix_now_secs};

pub(crate) use crate::rows::ops::list::{handle_workspace_activate, handle_workspace_list};

pub(crate) use crate::agents::ops::handle_accounts_list;

