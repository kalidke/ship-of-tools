//! The row: `Workspace`, its phase cell and the observations that move it.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use super::*;
use crate::paths;

/// `Default` is `Stopped`: no observation yet reads as "never started".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Stopped,
    Starting,
    Ready,
    Ending,
    EndedNoRespawn,
    Terminal,
    Unreachable,
    Foreign,
}

impl Phase {
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Phase::Stopped => "stopped",
            Phase::Starting => "starting",
            Phase::Ready => "ready",
            Phase::Ending => "ending",
            Phase::EndedNoRespawn => "ended_no_respawn",
            Phase::Terminal => "terminal",
            Phase::Unreachable => "unreachable",
            Phase::Foreign => "foreign",
        }
    }

    /// `Terminal` latches for its supervisor, `EndedNoRespawn` for its
    /// voyage — see [`Workspace::apply_phase_observation`].
    fn is_latched(self) -> bool {
        matches!(self, Phase::EndedNoRespawn | Phase::Terminal)
    }
}

/// A capsule authority's identity (pid + creation time). Compared by
/// EQUALITY only, never ordering — two processes can share a creation tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SupervisorIdentity {
    pub pid: u32,
    pub created: u64,
}

#[derive(Debug, Clone)]
pub(crate) enum Observation {
    /// `None` voyage keeps the cell's last-seen voyage rather than clearing it.
    Phase { phase: Phase, supervisor: SupervisorIdentity, voyage: Option<uuid::Uuid> },
    Stopped,
    Foreign,
    Failed,
    /// A leg this daemon spawned exited terminal (69) before ANY
    /// authority ever claimed the row -- the supervisor died inside its
    /// own bootstrap (lane bind, parent-death lease), so no `status`
    /// was ever served and there is no identity to mark against.
    /// Applied ONLY to an empty cell: `begin_supervisor_epoch` never
    /// writes `None`, so a cell claimed at any point in this daemon's
    /// life is closed to it and a stale watchdog cannot latch a row it
    /// no longer owns. Latches like any `Terminal` -- the operator
    /// signal a bootstrap failure must leave behind, rather than a
    /// `stopped` row that re-spawns the same instant failure on every
    /// attach -- while leaving the cell claimable, so a genuine
    /// authority answering later adopts and clears it.
    TerminalUnclaimed,
}

/// One lock: a rejection check and the write it gates share one critical section.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PhaseCell {
    phase: Phase,
    supervisor: Option<SupervisorIdentity>,
    voyage: Option<uuid::Uuid>,
    /// Reaching 2 sets `Unreachable` (unless already latched).
    consecutive_failures: u8,
}

impl std::fmt::Debug for Workspace {
    // Custom Debug so the un-Debug-able resource handles don't infect
    // tracing macros. The handles are intentionally opaque — their
    // "is constructed yet" state is the only thing worth logging.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("workspace_id", &self.workspace_id)
            .field("slug", &self.slug)
            .field("label", &self.label)
            .field("project_root", &self.project_root)
            .field("session_name", &self.session_name)
            .field("created", &self.created)
            .field("autostart_claude", &self.autostart_claude)
            .field("agent", &self.agent())
            .field("agent_name", &self.agent_name())
            .field("task", &self.task)
            .field("runtime", &self.runtime)
            .field("account", &self.account())
            .field("agent_handle", &self.agent_handle())
            .field("phase", &self.phase())
            .field("watchdog_owner", &self.watchdog_owner())
            .field("activation_error", &self.activation_error())
            .field("files_mode_built", &self.files_mode.get().is_some())
            .field("concept_built", &self.concept.get().is_some())
            .field("kernel_built", &self.kernel.get().is_some())
            .field("repl_built", &self.repl.get().is_some())
            .finish()
    }
}

impl Workspace {
    /// Build a workspace whose resources are *not yet* constructed.
    pub fn meta_only(
        workspace_id: String,
        slug: String,
        label: String,
        project_root: PathBuf,
        session_name: String,
        created: i64,
        autostart_claude: bool,
        agent: String,
        agent_name: String,
        task: String,
    ) -> Self {
        Workspace {
            workspace_id,
            slug,
            label,
            project_root,
            session_name,
            created,
            autostart_claude,
            agent: Mutex::new(agent),
            agent_name: Mutex::new(agent_name),
            task,
            // Callers with a decided value set it on the returned row
            // (`load_toml`'s `runtime` key, `insert`, workspace.create).
            runtime: "capsule".to_string(),
            // Same pattern as `runtime` just above: a caller with a
            // decided value (`load_toml`'s `account` key, `insert`,
            // `workspace.create`'s own resolution) sets it after
            // construction. `""` here is the default account.
            account: Mutex::new(String::new()),
            agent_handle: Mutex::new(String::new()),
            phase_cell: Mutex::new(PhaseCell::default()),
            watchdog_owner: Mutex::new(None),
            activation_error: Mutex::new(None),
            files_mode: OnceLock::new(),
            concept: OnceLock::new(),
            kernel: OnceLock::new(),
            repl: OnceLock::new(),
            watcher: OnceLock::new(),
        }
    }

    /// The sot-comm handle this workspace's session has DECLARED (ADR
    /// 0046 decision 1) — `""` if never joined. The one reader for the
    /// interior-mutable `agent_handle` cell; see that field's own doc.
    pub fn agent_handle(&self) -> String {
        self.agent_handle.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn agent(&self) -> String {
        self.agent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Which account this row's agent spends right now -- `""` is the
    /// agent's own default config folder. Changes only through
    /// [`Workspaces::set_account`] (ADR 0046 decision 6).
    pub fn account(&self) -> String {
        self.account.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn agent_name(&self) -> String {
        self.agent_name.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Mutates in place, on the SAME `Arc`, rather than replacing it through `insert`.
    pub(super) fn reset_agent_in_place(&self) {
        *self.agent.lock().unwrap_or_else(|e| e.into_inner()) = "none".to_string();
        *self.agent_name.lock().unwrap_or_else(|e| e.into_inner()) = String::new();
    }

    pub fn phase(&self) -> Phase {
        self.phase_cell.lock().unwrap_or_else(|e| e.into_inner()).phase
    }

    /// The ONLY way `phase_cell.supervisor` changes. Resets phase/voyage/failure-count
    /// fresh — the only thing that clears a `Terminal` latch.
    pub(crate) fn begin_supervisor_epoch(&self, identity: SupervisorIdentity) {
        let mut cell = self.phase_cell.lock().unwrap_or_else(|e| e.into_inner());
        *cell = PhaseCell { supervisor: Some(identity), ..PhaseCell::default() };
    }

    /// Whether a watchdog currently owns this row's restarts — see the
    /// field's own doc. Read by [`crate::rows::run::activation::
    /// resume_locked`] before ever spawning a resume.
    pub(crate) fn watchdog_owner(&self) -> Option<u64> {
        *self.watchdog_owner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The watchdog task's own announcement, once, at install — a plain
    /// overwrite: the caller is by construction the newest watchdog, and
    /// its token is unique for this daemon's lifetime.
    pub(crate) fn set_watchdog_owner(&self, token: u64) {
        *self.watchdog_owner.lock().unwrap_or_else(|e| e.into_inner()) = Some(token);
    }

    /// Compare-and-clear: `None`s the field only if it still holds
    /// `expected` — a superseded watchdog's own belated cleanup can
    /// never erase a replacement's ownership set after it.
    pub(crate) fn clear_watchdog_owner_if(&self, expected: u64) {
        let mut cell = self.watchdog_owner.lock().unwrap_or_else(|e| e.into_inner());
        if *cell == Some(expected) {
            *cell = None;
        }
    }

    pub(crate) fn current_supervisor(&self) -> Option<SupervisorIdentity> {
        self.phase_cell.lock().unwrap_or_else(|e| e.into_inner()).supervisor
    }

    /// The observer's single write path. One sentence: an EMPTY cell
    /// takes the first prover; a non-empty cell changes epoch only
    /// under the row guard (through [`Workspace::begin_supervisor_epoch`]).
    /// So a `Phase` observation is rejected unless `supervisor` EQUALS
    /// the cell's current epoch (never an ordering comparison) -- with
    /// `supervisor: None` meaning "unclaimed", which the observation
    /// itself then claims. Within an epoch, phases are voyage-ordered
    /// and `Terminal`/`EndedNoRespawn` latches reject anything but a
    /// strictly newer voyage.
    pub(crate) fn apply_phase_observation(&self, observation: Observation) -> bool {
        let mut cell = self.phase_cell.lock().unwrap_or_else(|e| e.into_inner());
        match observation {
            Observation::Phase { phase, supervisor, voyage } => {
                // An unclaimed cell adopts its first prover, on exactly
                // the terms `begin_supervisor_epoch` would -- a fresh
                // epoch, judged below like any other. This is what keeps
                // the post-spawn settle bound BEST-EFFORT rather than
                // correctness-critical: a supervisor that first answers
                // after the deadline is adopted by the next background
                // poll instead of being rejected forever.
                if cell.supervisor.is_none() {
                    *cell = PhaseCell { supervisor: Some(supervisor), ..PhaseCell::default() };
                }
                if cell.supervisor != Some(supervisor) {
                    return false;
                }
                if phase == Phase::Terminal {
                    cell.phase = Phase::Terminal;
                    if let Some(v) = voyage {
                        cell.voyage = Some(v);
                    }
                    cell.consecutive_failures = 0;
                    return true;
                }
                if cell.phase == Phase::Terminal {
                    return false;
                }
                if let (Some(v), Some(held_v)) = (voyage, cell.voyage) {
                    if v < held_v {
                        return false;
                    }
                }
                if cell.phase == Phase::EndedNoRespawn
                    && !matches!((voyage, cell.voyage), (Some(v), Some(held_v)) if v > held_v)
                {
                    return false;
                }
                cell.phase = phase;
                if let Some(v) = voyage {
                    cell.voyage = Some(v);
                }
                cell.consecutive_failures = 0;
                true
            }
            Observation::Stopped | Observation::Foreign => {
                if cell.phase.is_latched() {
                    return false;
                }
                cell.phase = if matches!(observation, Observation::Stopped) { Phase::Stopped } else { Phase::Foreign };
                cell.consecutive_failures = 0;
                true
            }
            // Claimed rows are closed to it -- see the variant's own doc.
            Observation::TerminalUnclaimed => {
                if cell.supervisor.is_some() {
                    return false;
                }
                cell.phase = Phase::Terminal;
                cell.consecutive_failures = 0;
                true
            }
            Observation::Failed => {
                if cell.phase.is_latched() {
                    return false;
                }
                cell.consecutive_failures = cell.consecutive_failures.saturating_add(1);
                if cell.consecutive_failures >= 2 {
                    cell.phase = Phase::Unreachable;
                }
                true
            }
        }
    }

    pub fn activation_error(&self) -> Option<String> {
        self.activation_error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Never touches `phase`.
    pub fn set_activation_error(&self, detail: Option<String>) {
        *self.activation_error.lock().unwrap_or_else(|e| e.into_inner()) = detail;
    }


    /// Lazily get this workspace's `FilesMode`, constructing it (and
    /// stating the project root) on first access. Subsequent calls
    /// return the cached `Arc`. Errors only on the *first* construction
    /// — once cached we re-emit `Ok` without re-stat'ing.
    pub fn files_mode(&self) -> Result<Arc<FilesMode>> {
        if let Some(fm) = self.files_mode.get() {
            return Ok(fm.clone());
        }
        let fm = Arc::new(FilesMode::new(self.project_root.clone())?);
        // OnceLock::set silently fails if another thread won the race;
        // either way we resolve through `get` again so both threads see
        // the same instance.
        let _ = self.files_mode.set(fm);
        Ok(self
            .files_mode
            .get()
            .expect("files_mode set/get race resolved")
            .clone())
    }

    /// Lazily get this workspace's `ConceptStore`. Rooted at the
    /// workspace's `project_root/.concept/` exactly as today's single-
    /// store backend did, just per-workspace.
    pub fn concept(&self) -> Arc<ConceptStore> {
        self.concept
            .get_or_init(|| Arc::new(ConceptStore::new(&self.project_root)))
            .clone()
    }

    /// Lazily get this workspace's `Kernel` handle. The Julia child is
    /// spawned lazily by the Kernel itself on first `request` — this
    /// just constructs the handle (no child yet) so per-workspace
    /// kernel state is correctly isolated when ops start routing.
    pub fn kernel(&self) -> Kernel {
        self.kernel
            .get_or_init(|| {
                Kernel::new(Kernel::default_kernel_project(), self.project_root.clone())
            })
            .clone()
    }

    /// Lazily get this workspace's `Repl` handle. Like Kernel above,
    /// the Julia child is spawned on first eval — this just gives us a
    /// per-workspace REPL identity so `x = 5` in workspace A doesn't
    /// leak into workspace B. `frame_tx` is the per-backend broadcast sink
    /// for streamed `repl.frame` evts; the Repl stamps this workspace's id
    /// onto every frame so the frontend routes them to the right drawer.
    /// Threaded in by the caller (`workspaces.repl_frame_tx()`) so the
    /// registry doesn't have to own the bus before startup wires it.
    pub fn repl(&self, frame_tx: broadcast::Sender<ReplFrameMsg>) -> Repl {
        self.repl
            .get_or_init(|| {
                // Default the REPL into THIS workspace's own project (its
                // Project.toml dir) so user code runs in the session package's
                // env, not the ShipToolsRepl shim. Only when the workspace has
                // no Project.toml do we leave it None (shim-only fallback).
                let user_project = self
                    .project_root
                    .join("Project.toml")
                    .is_file()
                    .then(|| self.project_root.clone());
                Repl::new(frame_tx, Some(self.workspace_id.clone()), user_project)
            })
            .clone()
    }

    /// Whether the Kernel handle has been constructed yet. Reflects
    /// in-memory state only; a kernel that died silently still shows
    /// `true` until the next request notices. Consumed by `workspace.list`
    /// to populate the `kernel_running` flag without paying a probe cost.
    pub fn kernel_built(&self) -> bool {
        self.kernel.get().is_some()
    }

    /// Lifecycle of this workspace's persistent REPL child, as a wire word
    /// (`not_started`/`starting`/`ready`/`dead`). `not_started` when the
    /// `Repl` handle was never constructed — the pre-first-eval norm.
    /// Consumed by `workspace.list` (`repl_state`) so a precompiling first
    /// boot renders as *starting* rather than dead; no probe cost — the
    /// supervisor maintains the state, this only reads the cell.
    pub fn repl_state(&self) -> &'static str {
        self.repl
            .get()
            .map(|r| r.state().as_str())
            .unwrap_or(crate::sidecars::repl::lifecycle::ReplLifecycle::NotStarted.as_str())
    }
}

impl Workspace {
    /// Construct a workspace from a label + project_root, deriving slug
    /// and tmux session name from the conventions in `paths`. Used both
    /// for fresh `workspace.create` calls and the default workspace at
    /// daemon startup (label = the `--label` arg or derived from
    /// project_root basename). Resources are lazy. `autostart_claude`
    /// is supplied by the caller — the create handler threads it from
    /// the request; the default startup workspace passes `false`.
    pub fn from_label(
        label: &str,
        project_root: PathBuf,
        autostart_claude: bool,
        agent: String,
        agent_name: String,
        task: String,
    ) -> Self {
        let slug = paths::slug(label);
        let session_name = super::session_name(label);
        let workspace_id = format!(
            "ws-{slug}-{:x}",
            std::process::id() as u64 ^ now_unix() as u64
        );
        // `agent_name` is stored EXACTLY as given — empty is a real,
        // supported case (capsule-comm-identity fix, Codex round finding
        // 2): no synthesized default is written here. A capsule with no
        // explicit `agent_name` gets no `SOT_COMM_NAME` pin either
        // (`agents::env::capsule_supervisor_env`); comm-join.sh's
        // own #148 auto-disambiguating derivation decides its handle, via
        // the `SOT_COMM_SELF_FILE` that spawn pins.
        Workspace::meta_only(
            workspace_id,
            slug,
            label.to_string(),
            project_root,
            session_name,
            now_unix(),
            autostart_claude,
            agent,
            agent_name,
            task,
        )
    }
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_from_label_uses_slug_and_tmux_convention() {
        let ws = Workspace::from_label("MyPkg.jl", PathBuf::from("/home/u/MyPkg.jl"), false, "none".into(), String::new(), String::new());
        assert_eq!(ws.slug, "mypkg_jl");
        assert_eq!(ws.session_name, "sot-be-mypkg_jl");
        assert_eq!(ws.label, "MyPkg.jl");
        assert_eq!(ws.project_root, PathBuf::from("/home/u/MyPkg.jl"));
    }

    #[test]
    fn workspace_kernel_built_reflects_lazy_construction() {
        // Fresh workspace: handle not yet constructed.
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        assert!(!ws.kernel_built());
        // Calling .kernel() constructs the handle (no child spawned yet).
        let _ = ws.kernel();
        assert!(ws.kernel_built());
    }

    #[test]
    fn every_phase_has_a_distinct_wire_string() {
        let all = [
            Phase::Stopped,
            Phase::Starting,
            Phase::Ready,
            Phase::Ending,
            Phase::EndedNoRespawn,
            Phase::Terminal,
            Phase::Unreachable,
            Phase::Foreign,
        ];
        let strings: std::collections::HashSet<&str> = all.iter().map(|p| p.as_wire_str()).collect();
        assert_eq!(strings.len(), all.len(), "every Phase must have its own distinct wire string");
    }
}
