// capsule_workspace.rs — ADR 0042 slice L1a / ADR 0043 decision 22: the
// daemon's capsule workspace runtime, on Windows AND Linux. One
// `sot-capsule supervise <state-dir>` authority per capsule workspace,
// spawned DETACHED so it survives the daemon's own exit — the daemon is
// never its kill domain. Which platform is chosen is exactly TWO forks
// inside `mod runtime` (the capsule executable's name and the detach
// mechanism — that module's own doc says why this used to be three)
// — everything else in that module is byte-identical on both platforms.
// `runtime: "tmux"`
// rows stay exactly what they are today; this module never touches
// them. ADR 0042's rule now holds on Linux too (L6 / this repo's B6
// lane, once the bridge gave a capsule row a remote attach path):
// `workspace.create`'s absent `runtime` resolves to "capsule" here
// just as it does on Windows; `"tmux"` still exists for an operator's
// explicit ask, and old rows retire by attrition.
//
// Split deliberately into PURE helpers (no OS call: the state-dir path
// arithmetic, the phase-to-wire-string mapping, the agent argv choice)
// and the platform runtime (spawning, watching, querying, ending a
// supervisor over `sot_log::supervisor_client`). The pure half is
// compiled and unit-tested on every platform — ADR 0042 L1a's own gate
// runs `cargo test --workspace` on Linux, and gating path/string
// arithmetic behind `#[cfg(windows)]` would only prevent that gate from
// ever exercising it. On a host that is neither Windows nor Linux
// nothing in this module is called at all: `workspace.create` keeps
// today's tmux path unchanged (see `workspaces.rs`/`handlers.rs`).


pub(crate) use crate::rows::run::headless;
pub(crate) use crate::rows::run::observer;
pub(crate) use crate::rows::run::activation::{ensure_started, resume_if_absent, resume_locked, ActivationIntent};
pub(crate) use crate::rows::run::end_run::{end_run, EndRunOutcome};
pub(crate) use crate::rows::run::probe::{phase_of, phase_str, UNREACHABLE_PHASE};
pub(crate) use crate::rows::run::resume::{resume_all, LANE_CONCURRENCY};
pub(crate) use crate::rows::run::start::{reset_run, start_supervisor};

pub use crate::agents::argv::agent_argv;
#[cfg(unix)]
pub use crate::agents::argv::agent_exec_argv;
pub use crate::agents::argv::claude_resume_argv;
#[cfg(not(windows))]
pub use crate::agents::env::agent_env;
pub use crate::agents::env::{capsule_supervisor_env, NESTING_ENV_VARS_TO_SCRUB};

pub(crate) use crate::rows::spawn::detach::{capsule_sibling_present, StartMode};
#[cfg(target_os = "macos")]
pub(crate) use crate::rows::spawn::state_root::macos_only;
pub(crate) use crate::rows::spawn::state_root::{
    qualified_state_root, state_dir_for, state_root_inside_project, STATE_ROOT_HINT,
};
