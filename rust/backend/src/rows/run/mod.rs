//! The daemon's view of a capsule row's run: its phase vocabulary, lifecycle observer, headless client, and the start, end, watchdog and resume paths.

pub(crate) mod end;
pub(crate) mod probe;

/// One lifecycle observer per capsule row -- the SINGLE writer of `Workspace::phase`.
pub(crate) mod observer;

/// ADR 0042 amendment (2026-09-07), "a session types into and reads a
/// sibling row": the daemon's own HEADLESS client on a capsule lane — the
/// same [`sot_log::attach_client::client::FeAttachClient`] the frontend's drawer
/// uses, run on the daemon side with no viewport and no user watching.
/// `type_into` takes the pen only long enough to deliver ONE `input` frame
/// and never resizes the pane (ADR 0041's take-on-first-input semantics,
/// applied to a second kind of client); `screen_of` attaches as a pure
/// WATCHER and never takes at all. Platform-neutral: the module is
/// declared without a platform gate.
pub(crate) mod headless;

pub(crate) mod activation;
pub(crate) mod end_run;
pub(crate) mod resume;
pub(crate) mod start;
pub(crate) mod watchdog;

#[cfg(target_os = "linux")]
use crate::rows::spawn::row_scope;
use crate::rows::spawn::state_root::state_dir_for;
use end_run::EndRunOutcome;
use probe::{local_phase, phase_for_missing_pointer, phase_str, FOREIGN_PHASE, NEVER_STARTED_PHASE, UNREACHABLE_PHASE};
