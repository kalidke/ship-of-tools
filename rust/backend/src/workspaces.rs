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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use tokio::sync::broadcast;

use crate::concept::ConceptStore;
use crate::files_mode::FilesMode;
use crate::kernel::Kernel;
use crate::paths;
use crate::repl::{Repl, ReplFrameMsg};
use crate::watcher::{PreviewChanged, Watcher};

/// One deduplicated workspace lifecycle event. The daemon broadcasts one
/// per successful create/destroy; each connection turns it into a
/// `workspace.changed` evt frame. Mirrors `watcher::PreviewChanged`.
#[derive(Clone, Debug)]
pub struct WorkspaceChanged {
    pub action: String,
    pub slug: String,
    pub workspace_id: String,
}

/// One relayed agent-to-agent message. The daemon broadcasts one per
/// `agent.send`; each connection turns it into an `agent.message` evt
/// frame. Mirrors `WorkspaceChanged` — a small Clone+Debug payload type
/// fanned out over a `broadcast::channel`. `to == ""` means broadcast.
/// `ts` is an ISO-8601 UTC string stamped by the daemon on receipt.
#[derive(Clone, Debug)]
pub struct AgentMessage {
    pub from: String,
    pub to: String,
    pub text: String,
    pub ts: String,
}

/// One workspace = one project under daemon supervision. The struct
/// owns both metadata (id, slug, label, paths) and lazily-constructed
/// per-workspace resources (file walker, concept store, kernel, repl).
/// Wrapped in `Arc` inside the registry so handler code can hold a
/// stable reference across an op.
///
/// Resources are `OnceLock`-cached: a workspace that's never the active
/// target of an op pays no construction cost beyond its toml entry. The
/// Kernel and Repl `OnceLock`s gate the *handles*; their Julia child
/// processes are spawned on first request inside each handle, so the
/// full chain is `workspace seen → handle constructed → child spawned`
/// — only the first step happens at workspace creation time.
pub struct Workspace {
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub project_root: PathBuf,
    pub tmux_session: String,
    pub created: i64,
    /// Whether the frontend should launch claude on first attach to this
    /// workspace's session. Plain metadata field; persisted in the toml
    /// and defaulted to false for older tomls that lack the key.
    pub autostart_claude: bool,
    /// Which agent this workspace auto-starts (ADR 0031): "claude" |
    /// "codex" | "none". Persisted; drives the boot wrapper's launcher
    /// branch (ccb / ccx / none) and the FE's row sigil.
    pub agent: String,
    /// The sot-comm handle the spawned agent should join as. Plain
    /// metadata; persisted in the toml and defaulted to "" when absent.
    pub agent_name: String,
    /// The initial instruction the FE delivers to the spawned agent after
    /// auto-starting claude. Plain metadata; persisted in the toml and
    /// defaulted to "" when absent.
    pub task: String,
    /// ADR 0042 slice L1a: `"tmux"` | `"capsule"` — which runtime hosts
    /// this workspace's agent pane. Plain metadata; persisted in the toml
    /// and defaulted to `"tmux"` for every existing/older toml that lacks
    /// the key — byte-for-byte today's behaviour for them. `"capsule"`
    /// workspaces are Windows-only in this unit (`handle_workspace_create`
    /// is the only writer of `"capsule"`); `tmux_session` is still
    /// populated for them (the same `sot-be-<slug>` convention) even
    /// though no real tmux session is ever created — it stays the one
    /// stable identifier `pty.open`'s `target` field addresses a
    /// workspace by, for both runtimes uniformly.
    pub runtime: String,
    files_mode: OnceLock<Arc<FilesMode>>,
    concept: OnceLock<Arc<ConceptStore>>,
    kernel: OnceLock<Kernel>,
    repl: OnceLock<Repl>,
    /// This workspace's file watcher (2026-07-10 multiwatch fix: previously
    /// only the default workspace was watched, so every other workspace's
    /// nav never live-refreshed). Spawned at registration when the watch
    /// bus is installed; `Some(None)` records a failed spawn so we don't
    /// retry per-op. Holding the Arc keeps the notify watcher alive for the
    /// workspace's lifetime; a re-insert drops the old entry (and thus its
    /// watcher) and spawns fresh.
    watcher: OnceLock<Option<Arc<Watcher>>>,
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
            .field("tmux_session", &self.tmux_session)
            .field("created", &self.created)
            .field("autostart_claude", &self.autostart_claude)
            .field("agent", &self.agent)
            .field("agent_name", &self.agent_name)
            .field("task", &self.task)
            .field("runtime", &self.runtime)
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
        tmux_session: String,
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
            tmux_session,
            created,
            autostart_claude,
            agent,
            agent_name,
            task,
            // Every existing constructor call site predates ADR 0042 L1a
            // and means "an ordinary tmux workspace" — see this field's
            // own doc. Callers that need `"capsule"` set it explicitly on
            // the returned value (workspace.create's Windows branch;
            // `load_toml`'s canonical-toml `runtime` key).
            runtime: "tmux".to_string(),
            files_mode: OnceLock::new(),
            concept: OnceLock::new(),
            kernel: OnceLock::new(),
            repl: OnceLock::new(),
            watcher: OnceLock::new(),
        }
    }

    /// Lazily get this workspace's `FilesMode`, constructing it (and
    /// stating the project root) on first access. Subsequent calls
    /// return the cached `Arc`. Errors only on the *first* construction
    /// — once cached we re-emit `Ok` without re-stat'ing.
    #[allow(dead_code)] // wired by handler routing (task #18)
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
    #[allow(dead_code)] // wired by handler routing (task #18)
    pub fn concept(&self) -> Arc<ConceptStore> {
        self.concept
            .get_or_init(|| Arc::new(ConceptStore::new(&self.project_root)))
            .clone()
    }

    /// Lazily get this workspace's `Kernel` handle. The Julia child is
    /// spawned lazily by the Kernel itself on first `request` — this
    /// just constructs the handle (no child yet) so per-workspace
    /// kernel state is correctly isolated when ops start routing.
    #[allow(dead_code)] // wired by handler routing (task #18)
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
    #[allow(dead_code)] // wired by handler routing (task #18)
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
                Repl::new(
                    Repl::default_repl_project(),
                    frame_tx,
                    Some(self.workspace_id.clone()),
                    user_project,
                )
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
            .unwrap_or(crate::repl::ReplLifecycle::NotStarted.as_str())
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
        let tmux_session = paths::tmux_session_name(label);
        let workspace_id = format!(
            "ws-{slug}-{:x}",
            std::process::id() as u64 ^ now_unix() as u64
        );
        // `agent_name` is stored EXACTLY as given — empty is a real,
        // supported case (capsule-comm-identity fix, Codex round finding
        // 2): no synthesized default is written here. A capsule with no
        // explicit `agent_name` gets no `SOT_COMM_NAME` pin either
        // (`capsule_workspace::capsule_supervisor_env`); comm-join.sh's
        // own #148 auto-disambiguating derivation decides its handle, via
        // the `SOT_COMM_SELF_FILE` that spawn pins.
        Workspace::meta_only(
            workspace_id,
            slug,
            label.to_string(),
            project_root,
            tmux_session,
            now_unix(),
            autostart_claude,
            agent,
            agent_name,
            task,
        )
    }
}

/// Shared registry. Wrapped in `RwLock` because handlers read on every
/// op but mutation is rare (workspace.create / workspace.destroy /
/// startup scan). Cloning a `Workspaces` clones the Arc.
#[derive(Clone, Default)]
pub struct Workspaces {
    inner: Arc<RwLock<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// Keyed by workspace_id (stable across daemon restarts because we
    /// persist it). `slug → workspace_id` index avoids a linear scan
    /// when a client refers to a workspace by slug.
    by_id: HashMap<String, Arc<Workspace>>,
    by_slug: HashMap<String, String>,
    /// The workspace requests resolve to when no `workspace_id` is
    /// supplied. Set at startup to the workspace matching the daemon's
    /// `--project-root` (and `--label`, if given). Required for
    /// back-compat with single-workspace clients.
    default_id: Option<String>,
    /// Per-backend broadcast sink for streamed `repl.frame` evts. Set once at
    /// startup (`set_repl_frame_tx`) from the sender `run()` creates; cloned
    /// out per eval (`repl_frame_tx`) and handed to `Workspace::repl` so a
    /// freshly-constructed per-workspace Repl publishes onto the same bus
    /// every connection subscribes to. `None` only in the window before
    /// startup wires it (and in the `Default` impl used by tests).
    repl_frame_tx: Option<broadcast::Sender<ReplFrameMsg>>,
    /// Server-monitoring hub (ADR 0020): always-on samplers + tiered ring +
    /// the `monitor.tick` broadcast bus. Installed once at startup
    /// (`set_monitor_hub`); each connection clones it out to subscribe and the
    /// `monitor.*` ops query its history. `None` only before startup wires it
    /// (and in the `Default` impl used by tests).
    monitor_hub: Option<crate::monitor::MonitorHub>,
    /// Pane-derived agent work-state cache (ADE state-nav), keyed by
    /// `tmux_session`. A background task (spawned in `server::run`) captures
    /// each workspace's live claude pane every ~2s and writes the derived
    /// activity ("working" / "idle" / "" for no-claude) here. `workspace.list`
    /// reads it as the authoritative working/idle signal — the `Stop`-hook
    /// `comm-status idle` only ever reports idle, so an actively-generating
    /// agent reads idle without this. Empty until the first capture tick.
    pane_activity: HashMap<String, String>,
    /// The shared preview.changed bus for per-workspace watcher spawns
    /// (2026-07-10 multiwatch). Installed once at startup via
    /// `set_watch_bus`, before workspace registration; `None` in tests. No
    /// longer paired with a `Session` handle (2026-09 rework): `preview.changed`
    /// is no longer bumped onto the session ring — see `watcher.rs`'s header
    /// comment.
    watch_bus: Option<broadcast::Sender<PreviewChanged>>,
    /// ADR 0042 slice L1a (Codex review finding 6): workspace ids whose
    /// capsule supervisor watchdog gave up — the ADR 0041 launcher
    /// restart sequence (1/3/7/15/30s, at most 5 in 60s) exhausted
    /// without a live authority, OR (rule F, shrink round) a leg exited
    /// terminal (69) outright with no restart attempted at all.
    /// `workspace.list` reads this BEFORE ever querying that workspace's
    /// (confirmed-gone) lane again, reporting `phase: "terminal"` loudly
    /// rather than a misleading fresh `"unreachable"` that implies the
    /// next probe might succeed. Never cleared automatically — only a
    /// fresh `workspace.create`/resume (a new watchdog) or daemon restart
    /// starts over. Rule E (shrink round): a `pty.open` start-on-attach
    /// spawn failure does NOT mark this — that would conflate "the
    /// supervisor itself reached a terminal outcome" with "one launch
    /// attempt failed"; the row stays retryable and the caller answers
    /// `capsule_spawn_failed` instead.
    capsule_terminal: std::collections::HashSet<String>,
    /// Rule D (shrink round): workspace ids with a capsule supervisor
    /// launch CURRENTLY in flight — inserted atomically by
    /// `Workspaces::try_begin_capsule_start` (`capsule_workspace::
    /// start_supervisor`'s own first act) and removed by
    /// `end_capsule_start` once the lane first answers or the leg exits
    /// (see `capsule_workspace::spawn_and_watch`'s own doc for the two
    /// independent, idempotently-converging release paths). The
    /// invariant this serves: AT MOST ONE launch per workspace at a
    /// time — `ensure_started`, `workspace.create`, and `resume_all` all
    /// check `is_capsule_starting` before attempting a spawn, so a
    /// second concurrent request (the exact race a fast `pty.open`
    /// against the daemon's own startup resume-scan can produce) spawns
    /// nothing and the caller answers normally, letting the requester's
    /// own retry find the lane once it comes up.
    starting: std::collections::HashSet<String>,
}

impl Workspaces {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a workspace. Idempotent on slug: if an entry exists for
    /// the same slug we keep its workspace_id (a stable id across daemon
    /// restarts is the contract), but the rest of the metadata is taken
    /// from the new `ws` so a fresh project_root from disk wins over a
    /// stale one in memory. Resource caches on the *old* entry are
    /// discarded — the assumption is that re-insertion happens at most
    /// once at startup (scan_disk) and during explicit workspace
    /// metadata edits, neither of which is on a hot path.
    pub fn insert(&self, ws: Workspace) -> Arc<Workspace> {
        let mut g = self.inner.write().expect("workspaces lock");
        let preserved_id = g
            .by_slug
            .get(&ws.slug)
            .cloned()
            .and_then(|id| g.by_id.get(&id).map(|w| w.workspace_id.clone()));
        let final_ws = match preserved_id {
            Some(id) => {
                // Same slug → keep id, new metadata wins.
                let mut w = Workspace::meta_only(
                    id,
                    ws.slug.clone(),
                    ws.label.clone(),
                    ws.project_root.clone(),
                    ws.tmux_session.clone(),
                    ws.created,
                    ws.autostart_claude,
                    ws.agent.clone(),
                    ws.agent_name.clone(),
                    ws.task.clone(),
                );
                // `meta_only` defaults `runtime` to "tmux" — the incoming
                // `ws`'s own value (not that default) is the new metadata
                // that should win here, same as every other field above.
                w.runtime = ws.runtime.clone();
                w
            }
            None => ws,
        };
        let arc = Arc::new(final_ws);
        g.by_slug
            .insert(arc.slug.clone(), arc.workspace_id.clone());
        g.by_id.insert(arc.workspace_id.clone(), arc.clone());
        if let Some(tx) = g.watch_bus.clone() {
            Self::spawn_workspace_watcher(&arc, tx);
        }
        arc
    }

    /// Spawn `ws`'s file watcher onto the shared bus. Cheap inline
    /// (`Watcher::spawn` opens the inotify fd and defers the recursive
    /// registration walk to a background thread — the NFS-stall hardening),
    /// so calling under the registry lock is fine. Failure is a warning:
    /// previews still work, that workspace just won't live-refresh.
    fn spawn_workspace_watcher(ws: &Arc<Workspace>, tx: broadcast::Sender<PreviewChanged>) {
        if ws.watcher.get().is_some() {
            return; // already spawned (or recorded as failed)
        }
        let spawned = match ws.files_mode() {
            Ok(fm) => match Watcher::spawn(fm.root_path(), fm.clone(), tx, Some(ws.slug.clone())) {
                Ok(w) => Some(Arc::new(w)),
                Err(e) => {
                    tracing::warn!(slug = %ws.slug, error = %e,
                        "workspace watcher spawn failed; nav will not live-refresh here");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(slug = %ws.slug, error = %e,
                    "workspace watcher: files_mode init failed");
                None
            }
        };
        let _ = ws.watcher.set(spawned);
    }

    /// Install the watch bus and spawn watchers for every ALREADY-registered
    /// workspace (registration order at startup isn't guaranteed relative to
    /// bus creation). Idempotent per workspace via the `watcher` OnceLock.
    pub fn set_watch_bus(&self, tx: broadcast::Sender<PreviewChanged>) {
        let existing: Vec<Arc<Workspace>> = {
            let mut g = self.inner.write().expect("workspaces lock");
            g.watch_bus = Some(tx.clone());
            g.by_id.values().cloned().collect()
        };
        for ws in existing {
            Self::spawn_workspace_watcher(&ws, tx.clone());
        }
    }

    pub fn set_default(&self, workspace_id: &str) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.default_id = Some(workspace_id.to_string());
    }

    /// Install the per-backend `repl.frame` broadcast sender. Called once at
    /// startup, right after the channel is created in `run()`, before any
    /// connection is accepted.
    pub fn set_repl_frame_tx(&self, tx: broadcast::Sender<ReplFrameMsg>) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.repl_frame_tx = Some(tx);
    }

    /// Clone the per-backend `repl.frame` broadcast sender. Handed to
    /// `Workspace::repl` so a per-workspace Repl publishes onto the bus every
    /// connection subscribes to. Panics if called before `set_repl_frame_tx`
    /// — startup always wires it before serving, so a `None` here is a bug.
    pub fn repl_frame_tx(&self) -> broadcast::Sender<ReplFrameMsg> {
        let g = self.inner.read().expect("workspaces lock");
        g.repl_frame_tx
            .clone()
            .expect("repl_frame_tx set at startup")
    }

    /// Install the server-monitoring hub. Called once at startup, right after
    /// `MonitorHub::start`, before any connection is accepted.
    pub fn set_monitor_hub(&self, hub: crate::monitor::MonitorHub) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.monitor_hub = Some(hub);
    }

    /// Clone the server-monitoring hub, if installed. `None` before startup
    /// wires it (or in tests) so callers degrade gracefully.
    pub fn monitor_hub(&self) -> Option<crate::monitor::MonitorHub> {
        let g = self.inner.read().expect("workspaces lock");
        g.monitor_hub.clone()
    }

    /// Record the pane-derived work-state for `session` (a `tmux_session`).
    /// Written by the background pane-watch task each tick. `activity` is one
    /// of "working" / "idle" / "" — see `crate::server::pane_activity`.
    pub fn set_pane_activity(&self, session: &str, activity: &str) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.pane_activity
            .insert(session.to_string(), activity.to_string());
    }

    /// Latest pane-derived work-state for `session`. `""` when no capture has
    /// landed yet (or the workspace's pane carried no running-claude marker),
    /// which `workspace.list` treats as "fall back to the registry state".
    pub fn pane_activity(&self, session: &str) -> String {
        let g = self.inner.read().expect("workspaces lock");
        g.pane_activity.get(session).cloned().unwrap_or_default()
    }

    /// ADR 0042 slice L1a: mark `workspace_id`'s capsule supervisor
    /// watchdog as having given up — the restart budget exhausted with no
    /// live authority, or (rule F, shrink round) a leg exited terminal
    /// (69) with no restart attempted at all. See `Inner::
    /// capsule_terminal`'s own doc (rule E: a one-shot `pty.open`
    /// start-on-attach spawn failure does NOT call this).
    /// `#[cfg_attr(not(windows), allow(dead_code))]`: pure, portable
    /// bookkeeping, but its only real caller is the Windows-only
    /// watchdog (`capsule_workspace.rs`) — matching that module's own
    /// precedent for a function whose ONLY callers are windows-gated.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn mark_capsule_terminal(&self, workspace_id: &str) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.capsule_terminal.insert(workspace_id.to_string());
    }

    /// `true` iff `mark_capsule_terminal` was ever called for this id.
    pub fn is_capsule_terminal(&self, workspace_id: &str) -> bool {
        let g = self.inner.read().expect("workspaces lock");
        g.capsule_terminal.contains(workspace_id)
    }

    /// Clears a previously-marked terminal flag — a fresh
    /// `workspace.create`/resume (a new watchdog) starts over. No-op if
    /// never marked. Same windows-only-caller reasoning as
    /// `mark_capsule_terminal` above.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn clear_capsule_terminal(&self, workspace_id: &str) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.capsule_terminal.remove(workspace_id);
    }

    /// Rule D (shrink round): atomically claim `workspace_id`'s capsule
    /// launch slot — `true` iff THIS call is the one that gets to spawn
    /// (the id was absent from `Inner::starting` and is now present);
    /// `false` means another launch is already in flight and the caller
    /// must spawn nothing. `HashSet::insert`'s own return value IS the
    /// atomic check-and-set: no separate read-then-write under the same
    /// lock acquisition could race a concurrent caller between them, and
    /// this uses none — one `write()` guard, one `insert` call. The
    /// invariant this serves: at most one capsule supervisor launch per
    /// workspace at a time. `capsule_workspace::start_supervisor`'s own
    /// first act. `#[cfg_attr(not(windows), allow(dead_code))]`: same
    /// windows-only-caller reasoning as `mark_capsule_terminal` above —
    /// every real caller lives in `capsule_workspace.rs`'s
    /// `windows_runtime` module or `handlers.rs`'s `#[cfg(windows)]`
    /// capsule-create block.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn try_begin_capsule_start(&self, workspace_id: &str) -> bool {
        let mut g = self.inner.write().expect("workspaces lock");
        g.starting.insert(workspace_id.to_string())
    }

    /// Release `workspace_id`'s capsule launch slot claimed by
    /// [`try_begin_capsule_start`]. Idempotent: removing an absent id is
    /// a harmless no-op, which is exactly what lets TWO independent
    /// release paths (`capsule_workspace::spawn_and_watch`'s own doc:
    /// "the lane first answers or the child exits") call this without
    /// coordinating — whichever fires first actually clears the entry.
    /// Same windows-only-caller reasoning as `try_begin_capsule_start`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn end_capsule_start(&self, workspace_id: &str) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.starting.remove(workspace_id);
    }

    /// `true` iff a capsule launch is currently claimed for this id —
    /// the early-exit half of rule D's invariant (`ensure_started`,
    /// `workspace.create`, and `resume_all` all check this BEFORE
    /// attempting a spawn, to skip the work rather than race it out;
    /// `try_begin_capsule_start`'s own atomic insert is what actually
    /// enforces the invariant against a racer that read this a moment
    /// too early). Same windows-only-caller reasoning as
    /// `try_begin_capsule_start`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn is_capsule_starting(&self, workspace_id: &str) -> bool {
        let g = self.inner.read().expect("workspaces lock");
        g.starting.contains(workspace_id)
    }

    /// Current default workspace id, if one has been set. Consumed by
    /// `workspace.list` to mark the default entry; the frontend uses it
    /// to render a "(default)" badge and skip switch-back logic.
    pub fn default_id(&self) -> Option<String> {
        let g = self.inner.read().expect("workspaces lock");
        g.default_id.clone()
    }

    /// Resolve an optional workspace_id to a workspace handle. `None`
    /// → default. A non-default id that's missing is `None` (caller's
    /// responsibility to error). The returned `Arc` shares the same
    /// resource caches with all other holders.
    pub fn resolve(&self, id: Option<&str>) -> Option<Arc<Workspace>> {
        let g = self.inner.read().expect("workspaces lock");
        let key = id.map(|s| s.to_string()).or_else(|| g.default_id.clone())?;
        // The id might actually be a slug (we accept both). Try id
        // first, then slug-index.
        if let Some(ws) = g.by_id.get(&key) {
            return Some(ws.clone());
        }
        if let Some(real_id) = g.by_slug.get(&key) {
            return g.by_id.get(real_id).cloned();
        }
        None
    }

    pub fn list(&self) -> Vec<Arc<Workspace>> {
        let g = self.inner.read().expect("workspaces lock");
        let mut out: Vec<Arc<Workspace>> = g.by_id.values().cloned().collect();
        out.sort_by(|a, b| a.slug.cmp(&b.slug));
        out
    }

    /// Project root of the workspace that owns tmux session `target`, if
    /// any. The LLM-pane pty uses this to create `tmux new-session` with
    /// `-c <project_root>`: without it `new-session -A` roots the
    /// orchestrator's shell in the daemon's launch dir (commonly `$HOME`),
    /// which is the wrong workspace and an over-broad trust scope. `None`
    /// when `target` isn't a known workspace session (e.g. the home-base
    /// `sot-llm` default) — caller then omits `-c`.
    pub fn project_root_for_tmux(&self, target: &str) -> Option<std::path::PathBuf> {
        let g = self.inner.read().expect("workspaces lock");
        g.by_id
            .values()
            .find(|ws| ws.tmux_session == target)
            .map(|ws| ws.project_root.clone())
    }

    /// The workspace **slug** owning this tmux session (`sot-be-<slug>`),
    /// e.g. `"alpha"`. `None` for the home-base `sot-llm` default (no
    /// matching workspace). Stamped into a spawned session's env as
    /// `SOT_WORKSPACE` so a session in the pane knows which workspace it
    /// is in and can gate FE nav commands on it.
    pub fn slug_for_tmux(&self, target: &str) -> Option<String> {
        let g = self.inner.read().expect("workspaces lock");
        g.by_id
            .values()
            .find(|ws| ws.tmux_session == target)
            .map(|ws| ws.slug.clone())
    }

    /// The whole workspace owning `target` (the same identifier
    /// `project_root_for_tmux`/`slug_for_tmux` match against — see
    /// `Workspace::runtime`'s own doc for why a capsule workspace still
    /// has one). ADR 0042 slice L1a: `pty.open` uses this to check
    /// `runtime` BEFORE falling into any tmux logic, so a capsule
    /// workspace's target is refused early rather than handed to
    /// `Pty::spawn`.
    pub fn workspace_for_tmux(&self, target: &str) -> Option<Arc<Workspace>> {
        let g = self.inner.read().expect("workspaces lock");
        g.by_id.values().find(|ws| ws.tmux_session == target).cloned()
    }

    pub fn remove_by_id(&self, id: &str) -> Option<Arc<Workspace>> {
        let mut g = self.inner.write().expect("workspaces lock");
        let removed = g.by_id.remove(id)?;
        g.by_slug.remove(&removed.slug);
        if g.default_id.as_deref() == Some(id) {
            g.default_id = None;
        }
        Some(removed)
    }
}

/// Read every `*.toml` in `~/.config/sot/workspaces/` (and, for
/// migration, in `~/.config/sot/sessions/`) and insert into the
/// registry. Best-effort: a malformed toml is logged and skipped.
/// Returns the count inserted.
pub fn scan_disk(reg: &Workspaces) -> Result<usize> {
    // Windows only: adopt the legacy config dir's backend children first,
    // so the per-host migration right below operates on the NEW root's
    // contents rather than a since-abandoned old one. A failure here is a
    // boot error (`?` — see that function's doc for why), not a warning.
    #[cfg(windows)]
    migrate_legacy_windows_config_dir()?;
    // Per-host state dirs (see `state_host`): adopt the legacy unsuffixed
    // dirs on the first post-deploy boot, before scanning.
    migrate_legacy_state_dirs();
    let mut count = 0;
    let workspaces_dir = workspaces_dir();
    if workspaces_dir.is_dir() {
        count += scan_dir(reg, &workspaces_dir, false)?;
    }
    // ADR 0013 legacy: stamp-on-startup wrote `~/.config/sot/sessions/<slug>.toml`.
    // We still read those so a daemon upgrade doesn't lose adoptable workspaces;
    // they migrate to workspaces/ on next write.
    let sessions_dir = sessions_dir();
    if sessions_dir.is_dir() {
        count += scan_dir(reg, &sessions_dir, true)?;
    }
    Ok(count)
}

fn scan_dir(reg: &Workspaces, dir: &Path, legacy: bool) -> Result<usize> {
    let mut count = 0;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        match load_toml(&path, legacy) {
            Ok(Some(ws)) => {
                reg.insert(ws);
                count += 1;
            }
            Ok(None) => {
                tracing::debug!(toml = ?path, "skipping toml with no [backend] / workspace section");
            }
            Err(e) => {
                tracing::warn!(error = %e, toml = ?path, "could not parse workspace toml; skipping");
            }
        }
    }
    Ok(count)
}

/// Parse a workspace toml. We handle both shapes:
///
///   ADR 0014 (canonical): top-level `workspace_id`, `slug`, `label`,
///   `project_root`, `tmux_session`, optional `[kernel]`.
///
///   ADR 0013 legacy: `[backend]` section with `session_id`, `label`,
///   `project_dir`, `tmux_session`.
///
/// Returns `Ok(None)` for files that don't look like either (so we can
/// skip without erroring).
fn load_toml(path: &Path, legacy_ok: bool) -> Result<Option<Workspace>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {path:?}"))?;

    // First pass: top-level (ADR 0014) keys.
    let kv = parse_kv(&text);
    let canonical = kv.get("workspace_id").is_some()
        && kv.get("slug").is_some()
        && kv.get("project_root").is_some();
    if canonical {
        let workspace_id = kv.get("workspace_id").cloned().unwrap_or_default();
        let slug = kv.get("slug").cloned().unwrap_or_default();
        let label = kv.get("label").cloned().unwrap_or_else(|| slug.clone());
        // simplify_verbatim: a toml written by a pre-fix build can carry a
        // Windows `\\?\` verbatim root (`std::fs::canonicalize`'s own
        // return form there) — CreateProcess rejects that as a working
        // directory (`capsule supervisor spawn failed: The directory name
        // is invalid. (os error 267)`, field defect 2026-09-04). Normalize
        // on load so every consumer (capsule spawn's `current_dir`, the
        // awareness env, the tmux path, display, comm-identity root
        // compares) sees the plain form the daemon has always assumed. A
        // no-op on non-Windows and on already-plain paths.
        let project_root = paths::simplify_verbatim(PathBuf::from(
            kv.get("project_root").cloned().unwrap_or_default(),
        ));
        let tmux_session = kv
            .get("tmux_session")
            .cloned()
            .unwrap_or_else(|| paths::tmux_session_name(&label));
        let created = kv
            .get("created")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or_else(now_unix);
        // Older tomls predate this key → default false.
        let autostart_claude = kv
            .get("autostart_claude")
            .map(|s| s == "true")
            .unwrap_or(false);
        // Older tomls predate these keys → default "" / derive agent.
        let agent = kv.get("agent").cloned().unwrap_or_else(|| {
            if autostart_claude { "claude".into() } else { "none".into() }
        });
        let agent_name = kv.get("agent_name").cloned().unwrap_or_default();
        let task = kv.get("task").cloned().unwrap_or_default();
        // ADR 0042 slice L1a. Older tomls predate this key -> default to
        // this platform's ordinary workspace runtime: "tmux", matching
        // `meta_only`'s own default and preserving byte-for-byte
        // behaviour for every workspace that predates capsules — except
        // on Windows (Codex review, PR #175), where it's "capsule"
        // instead: tmux never runs there at all, so a toml migrated from
        // a pre-fix legacy registry (predating this key) defaulting to
        // "tmux" made the daemon try to secure a tmux socket dir before
        // failing to spawn a nonexistent `tmux.exe` (field evidence,
        // v0.6.0-rc.3 — see `paths::tmux_socket_path`'s doc).
        #[cfg(windows)]
        let default_runtime = "capsule";
        #[cfg(not(windows))]
        let default_runtime = "tmux";
        let runtime = kv.get("runtime").cloned().unwrap_or_else(|| default_runtime.to_string());
        let mut ws = Workspace::meta_only(
            workspace_id,
            slug,
            label,
            project_root,
            tmux_session,
            created,
            autostart_claude,
            agent,
            agent_name,
            task,
        );
        ws.runtime = runtime;
        return Ok(Some(ws));
    }

    if !legacy_ok {
        return Ok(None);
    }

    // Legacy pass: `[backend]` section keys.
    let backend = parse_section(&text, "backend");
    if backend.is_empty() {
        return Ok(None);
    }
    let label = backend.get("label").cloned().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    // simplify_verbatim: same normalization as the canonical branch above
    // — a legacy toml is just as capable of carrying a stale verbatim root.
    let project_root = paths::simplify_verbatim(PathBuf::from(
        backend
            .get("project_dir")
            .cloned()
            .unwrap_or_else(|| ".".into()),
    ));
    let slug = paths::slug(&label);
    let tmux_session = backend
        .get("tmux_session")
        .cloned()
        .unwrap_or_else(|| paths::tmux_session_name(&label));
    let workspace_id = backend
        .get("session_id")
        .cloned()
        .unwrap_or_else(|| format!("ws-{slug}-legacy"));
    let created = backend
        .get("started")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(now_unix);
    // Legacy [backend] tomls predate these keys → default false / "".
    Ok(Some(Workspace::meta_only(
        workspace_id,
        slug,
        label,
        project_root,
        tmux_session,
        created,
        false,
        "none".to_string(),
        String::new(),
        String::new(),
    )))
}

/// The single decision of what runtime the daemon's own default/home row
/// gets at boot, for this OS — the counterpart of `load_toml`'s per-OS
/// `default_runtime` (which fills in a MISSING key) extended to a
/// pre-existing WRONG one: on Windows the answer is unconditionally
/// `"capsule"`, `existing` or not — tmux is refused there outright
/// (#177, `TmuxClient::run`), so an on-disk `"tmux"` value is never a
/// legitimate steady state, only a leftover (a stale toml, an
/// older/pre-L1a writer, a manual edit). Preserving it verbatim (the old
/// behaviour) meant it never self-healed: the daemon then refused to
/// start the row (`pty spawn failed error=tmux is not available on
/// Windows`) and it can't be destroyed either
/// (`default_workspace_not_destroyable`) — a dead end. Every other host
/// preserves `existing` verbatim; a genuinely first-ever launch (no
/// `existing`) gets `first_launch_default` (`from_label`'s own "tmux").
pub(crate) fn default_row_runtime(existing: Option<&str>, first_launch_default: &str) -> String {
    if cfg!(windows) {
        "capsule".to_string()
    } else {
        existing.unwrap_or(first_launch_default).to_string()
    }
}

/// The single decision of what LAUNCH FIELDS (`autostart_claude`, `agent`,
/// `agent_name`, `task`) the daemon's own default/home row gets at boot —
/// the counterpart of [`default_row_runtime`] above, for the launch
/// fields rather than the runtime string.
///
/// 2026-09-04 amendment (owner ruling): the default/home row is an INERT
/// ANCHOR, not a session — the workspace the daemon falls back to and
/// the way to browse this machine's files. A genuinely first-ever launch
/// (`existing: None`) therefore seeds no agent and no autostart, on
/// EVERY host alike — no OS branch needed here any more (before this
/// amendment Windows alone seeded `agent = "claude"`,
/// `autostart_claude = true`, so pressing Enter on the row silently
/// started a claude capsule and it looked like every other session).
///
/// `existing` survives verbatim UNLESS it is a Windows CORRUPTED row —
/// `existing.0` (its on-disk `runtime`) is not `"capsule"`, the same
/// field incident [`default_row_runtime`]'s own doc describes: whatever
/// wrote a stale `"tmux"` there flipped `agent`/`autostart_claude`
/// alongside it, so preserving them verbatim would boot a capsule with a
/// corrupted agent. That row re-seeds to the SAME inert defaults a
/// first-ever launch gets, rather than carrying the corruption forward.
///
/// `existing` is `(runtime, autostart_claude, agent, agent_name, task)`
/// — the persisted row's own fields, read back before `Workspace::from_label`
/// would otherwise silently replace them (`insert`'s "new metadata wins"
/// semantics, `server::run`'s own doc).
pub(crate) fn default_row_launch_seed(
    existing: Option<(&str, bool, &str, &str, &str)>,
) -> (bool, String, String, String) {
    match existing {
        Some((runtime, autostart_claude, agent, agent_name, task))
            if !(cfg!(windows) && runtime != "capsule") =>
        {
            (
                autostart_claude,
                agent.to_string(),
                agent_name.to_string(),
                task.to_string(),
            )
        }
        _ => (false, "none".to_string(), String::new(), String::new()),
    }
}

/// Write `~/.config/sot/workspaces/<slug>.toml`. Frontend-managed
/// sections (`[nav_state]`, `[layout]`, …) that the file already
/// contains are preserved — same approach as `session_state.rs`.
pub fn save(ws: &Workspace) -> Result<PathBuf> {
    let target = workspaces_dir().join(format!("{}.toml", ws.slug));
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {parent:?}"))?;
    }
    let existing = std::fs::read_to_string(&target).unwrap_or_default();
    let preserved = strip_canonical_top_and_kernel(&existing);

    let mut body = String::new();
    body.push_str(&format!("workspace_id  = {}\n", toml_quote(&ws.workspace_id)));
    body.push_str(&format!("slug          = {}\n", toml_quote(&ws.slug)));
    body.push_str(&format!("label         = {}\n", toml_quote(&ws.label)));
    body.push_str(&format!(
        "project_root  = {}\n",
        toml_quote(&ws.project_root.to_string_lossy())
    ));
    body.push_str(&format!(
        "tmux_session  = {}\n",
        toml_quote(&ws.tmux_session)
    ));
    body.push_str(&format!("created       = {}\n", ws.created));
    body.push_str(&format!(
        "autostart_claude = {}\n",
        ws.autostart_claude
    ));
    body.push_str(&format!("agent         = {}\n", toml_quote(&ws.agent)));
    // agent_name / task are free text — quote+escape them exactly as
    // `label` is via `toml_quote` (handles quotes, backslashes, and
    // \n/\r/\t). The load side pairs `strip_quotes` with `toml_unquote`
    // (its inverse), so — same as `label` — an embedded `"` or newline
    // now round-trips exactly (field defect fixed 2026-09-04: the reader
    // used to only strip the surrounding quotes, leaving every escape
    // literal — see `toml_unquote`'s doc).
    body.push_str(&format!("agent_name    = {}\n", toml_quote(&ws.agent_name)));
    body.push_str(&format!("task          = {}\n", toml_quote(&ws.task)));
    body.push_str(&format!("runtime       = {}\n", toml_quote(&ws.runtime)));

    let final_text = if preserved.trim().is_empty() {
        body
    } else if preserved.ends_with('\n') {
        format!("{body}\n{preserved}")
    } else {
        format!("{body}\n{preserved}\n")
    };

    let tmp = target.with_extension("toml.tmp");
    std::fs::write(&tmp, final_text.as_bytes())
        .with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("rename {tmp:?} -> {target:?}"))?;
    Ok(target)
}

/// Short hostname for PER-HOST state dirs. Shared-$HOME cohorts (NFS) run one
/// daemon PER MACHINE; unsuffixed dirs made those daemons cross-contaminate —
/// a workspace toml written by any box resurrected on every other box's next
/// daemon boot (the gatecheck/canary incidents, and the reason "a BE on
/// a remote host" looked broken). First hostname label, lowercased;
/// `SOT_STATE_HOST` overrides for tests/exotic setups.
///
/// `pub(crate)` (capsule-comm-identity fix): also the host `comm-lib.sh`'s
/// own handles are suffixed with, so `capsule_workspace::capsule_supervisor_env`
/// reuses this SAME resolution for the capsule's `SOT_COMM_SELF_FILE`
/// path rather than re-deriving a host string that could drift from it.
pub(crate) fn state_host() -> String {
    if let Ok(h) = std::env::var("SOT_STATE_HOST") {
        if !h.is_empty() {
            return h;
        }
    }
    #[cfg(windows)]
    let raw = std::env::var("COMPUTERNAME").unwrap_or_default();
    #[cfg(not(windows))]
    let raw = std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_default();
    let short = raw.split('.').next().unwrap_or("").trim().to_lowercase();
    if short.is_empty() {
        "host".to_string()
    } else {
        short
    }
}

/// One-time migration: rename the legacy UNSUFFIXED state dirs to this
/// host's suffixed ones. Runs at daemon boot (from `load_all`, after
/// `migrate_legacy_windows_config_dir` on Windows); the first post-deploy
/// boot on the cohort inherits the legacy state (that's the primary dev box
/// — the only daemon that ever wrote it), every other host starts fresh, and
/// public single-home installs have nothing to migrate. Rename failures warn
/// and leave the legacy dir in place (nothing is destroyed).
///
/// The plain rename only covers "suffixed sibling absent yet". When it's
/// already THERE, this folds instead of skipping (`fold_unsuffixed_into_per_host`)
/// — field defect (2026-09-04): on Windows, `migrate_legacy_windows_config_dir`
/// can itself deposit an un-suffixed `workspaces`/`sessions` at the new root
/// — a SECONDARY legacy candidate's un-suffixed dir merges in verbatim-named
/// (see that function's secondary-candidate doc) — and if the PRIMARY
/// candidate (or an earlier boot's default-workspace write) already produced
/// the suffixed sibling at the new root first, the plain rename below used to
/// be a no-op: the secondary's rows sat un-suffixed at the new root forever,
/// in a directory `workspaces_dir()`/`sessions_dir()` never scans. A box that
/// already ran that broken migration has the identical shape stranded at the
/// new root from a prior boot — same fold, no separate recovery path, so
/// there is exactly one place that ever reads or writes an un-suffixed
/// registry dir under `app_config_dir()`.
pub(crate) fn migrate_legacy_state_dirs() {
    for name in ["workspaces", "sessions"] {
        let legacy = app_config_dir().join(name);
        let per_host = app_config_dir().join(format!("{name}-{}", state_host()));
        if !legacy.is_dir() {
            continue;
        }
        if !per_host.exists() {
            match std::fs::rename(&legacy, &per_host) {
                Ok(()) => tracing::info!(from = %legacy.display(), to = %per_host.display(),
                    "migrated legacy state dir to per-host (ADR 0013/0014 addendum)"),
                Err(e) => tracing::warn!(error = %e, from = %legacy.display(),
                    "legacy state dir migration failed — leaving in place"),
            }
            continue;
        }
        fold_unsuffixed_into_per_host(&legacy, &per_host);
    }
}

/// Fold every `.toml` sitting directly in `legacy` (an un-suffixed registry
/// dir) into `per_host` (its host-suffixed sibling, which already exists),
/// then remove `legacy` once it's empty. Same non-colliding merge rule as
/// `merge_secondary_legacy_windows_children`: a name already present at the
/// destination wins outright — that file is left at `legacy`, untouched, and
/// only warned about (naming both paths); nothing at `per_host` is ever
/// overwritten. Unlike that function, this one MOVES (not copies) each
/// non-colliding file and deletes the source directory when empty — `legacy`
/// here is a dir this daemon owns under its OWN new root, not an
/// still-independently-live legacy candidate root that must be left exactly
/// as found either way. Logs one line per moved toml (field-log
/// distinguishability, matching `migrate_legacy_windows_config_dir`'s own
/// convention). Best-effort: an unreadable `legacy` dir, or a failed
/// individual rename, is a warning, not a boot error — the daemon still
/// starts against whatever `per_host` alone resolves to.
fn fold_unsuffixed_into_per_host(legacy: &Path, per_host: &Path) {
    let entries = match std::fs::read_dir(legacy) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, dir = %legacy.display(),
                "could not read a stranded un-suffixed state dir to fold it into the per-host one");
            return;
        }
    };
    // Counts anything left behind at `legacy` — a name collision, a failed
    // rename, or a non-toml entry — so `legacy` is only removed once it's
    // genuinely empty, never a non-empty dir masquerading as folded.
    let mut left_behind = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            left_behind += 1;
            continue;
        }
        let Some(name) = path.file_name() else {
            left_behind += 1;
            continue;
        };
        let dst = per_host.join(name);
        if dst.exists() {
            tracing::warn!(from = %path.display(), to = %dst.display(),
                "a stranded un-suffixed registry has a toml with the same name as one \
                 already at the per-host dir — left in place, not folded (rename by hand to adopt it)");
            left_behind += 1;
            continue;
        }
        match std::fs::rename(&path, &dst) {
            Ok(()) => tracing::info!(from = %path.display(), to = %dst.display(),
                "folded a stranded un-suffixed registry toml into the per-host dir"),
            Err(e) => {
                tracing::warn!(error = %e, from = %path.display(), to = %dst.display(),
                    "could not fold a stranded un-suffixed registry toml into the per-host dir");
                left_behind += 1;
            }
        }
    }
    if left_behind == 0 {
        if let Err(e) = std::fs::remove_dir(legacy) {
            tracing::warn!(error = %e, dir = %legacy.display(),
                "folded every toml out of a stranded un-suffixed registry dir but could not remove it");
        }
    }
}

fn workspaces_dir() -> PathBuf {
    app_config_dir().join(format!("workspaces-{}", state_host()))
}

/// Path to a workspace's on-disk toml for the given slug. Mirrors
/// the layout `save()` writes to so handlers can find a file to
/// delete on `workspace.destroy`.
pub fn toml_path_for(slug: &str) -> PathBuf {
    workspaces_dir().join(format!("{slug}.toml"))
}

/// Legacy ADR-0013 session toml for the slug. `workspace.destroy` must
/// remove this too: startup migration re-registers any slug found here,
/// so a surviving legacy toml resurrects a destroyed workspace on every
/// daemon restart (observed: the immortal `montest` session, killed in
/// tmux repeatedly and re-created from its legacy toml each time).
pub fn legacy_toml_path_for(slug: &str) -> PathBuf {
    sessions_dir().join(format!("{slug}.toml"))
}

pub(crate) fn sessions_state_dir() -> PathBuf {
    sessions_dir()
}

fn sessions_dir() -> PathBuf {
    app_config_dir().join(format!("sessions-{}", state_host()))
}

/// App config dir: `~/.config/sot`. Shared so every backend config resolver
/// (workspaces, sessions, backend-identity) agrees on one dir.
///
/// Windows: delegates to `paths::windows_state_root()`
/// (`%LOCALAPPDATA%\sot`, or `%USERPROFILE%\AppData\Local\sot` — see that
/// function), joined with `config`, instead of the `config_dir()` logic
/// below — which resolves via `$HOME`, a POSIX-only env var with no
/// Windows branch. That was the actual defect (v0.6.0-rc.3 field report):
/// a git-bash shell exports `HOME` (`config_dir()` used to land on
/// `C:\Users\<u>\.config\sot`) while the PowerShell launcher does not (it
/// fell through to the `/tmp/.config` literal, which Windows path
/// handling turns into `\tmp\.config` on whatever the current drive is) —
/// so a hand-started daemon and a launcher-started one built and wrote to
/// TWO DIFFERENT registries, and the default workspace was created twice
/// with different ids, dropping rows between them. `config` keeps this a
/// sibling of `paths::state_dir()`'s `state` under the same
/// `%LOCALAPPDATA%\sot` root, chosen so neither collides with the capsule
/// runtime's own `workspaces\<id>` subtree
/// (`capsule_workspace::state_dir_for`). Mirrors
/// `sot_log::state_dir::sot_state_dir`'s own precedent of ignoring
/// `XDG_*` on Windows in favour of `%LOCALAPPDATA%` (its module doc:
/// letting a second env var win on Windows is exactly how the FE/capsule
/// state dirs drifted apart once already) — `$XDG_CONFIG_HOME` keeps
/// working here on Unix, unchanged, same as before this fix. NO fallback
/// to the `config_dir()` logic below on Windows any more (Codex review,
/// PR #175): `windows_state_root()` panics with a clear message instead
/// of silently landing on a `$HOME`-shaped path there.
pub(crate) fn app_config_dir() -> PathBuf {
    #[cfg(windows)]
    return crate::paths::windows_state_root().join("config");
    #[cfg(not(windows))]
    config_dir().join("sot")
}

#[cfg(not(windows))]
fn config_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(v);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config");
        return p;
    }
    PathBuf::from("/tmp/.config")
}

/// Directory entries directly under `root` matching the backend's OWN
/// registry dirs: `workspaces`, `sessions` (pre-per-host legacy shape) and
/// `workspaces-*`/`sessions-*` (current per-host shape, any host suffix —
/// `state_host()` isn't consulted here since migration runs before/
/// independent of which host string this boot resolves). Named
/// explicitly rather than swept wholesale because `<...>\.config\sot` is
/// NOT backend-exclusive on Windows: the frontend resolves its OWN files
/// there too (`settings.toml`, `keybindings.toml`, `hosts.toml`,
/// `state-<host>.toml`) — renaming the whole dir would silently reset the
/// frontend (Codex review finding). Sorted for deterministic iteration
/// order (used by both the rename loop and tests).
#[cfg(windows)]
fn backend_registry_children(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "workspaces"
            || name == "sessions"
            || name.starts_with("workspaces-")
            || name.starts_with("sessions-")
        {
            out.push(name.to_string());
        }
    }
    out.sort();
    out
}

/// Every root `app_config_dir()` (or its pre-fix predecessor) could have
/// resolved to on Windows, in the order this function probes them —
/// deduplicated by value (`HOME` and `USERPROFILE` commonly hold the same
/// path under Git for Windows; the current-drive and `%SystemDrive%` `tmp`
/// candidates likewise collapse when they're the same drive), so a value
/// reachable by two different env vars is probed, and migrated, exactly
/// once:
///   1. `<XDG_CONFIG_HOME>\sot` — `config_dir()`'s own first tier, honoured
///      on every platform pre-fix, including Windows.
///   2. `<HOME>\.config\sot` — a git-bash shell exports `HOME`; its value
///      there IS `%USERPROFILE%` (so this and #3 usually collapse to one
///      candidate after dedup, but not always — `HOME` can be overridden).
///   3. `<USERPROFILE>\.config\sot` — same shape, keyed off the env var a
///      real Windows login always sets (unlike `HOME`), so migration finds
///      the right root regardless of which shell THIS boot happens to be
///      launched from.
///   4. `\tmp\.config\sot` (current-drive-relative-root) — `config_dir()`'s
///      literal fallback when neither `XDG_CONFIG_HOME` nor `HOME` was
///      set; Windows roots a leading `/`/`\` with no drive prefix onto
///      whatever the process's current drive is.
///   5. `<SystemDrive>\tmp\.config\sot` — the same fallback, explicit-
///      drive form, for when the daemon's current drive isn't the system
///      drive (field-observed value on the box that WAS on the system
///      drive: `wrote backend identity toml
///      toml="/tmp/.config\sot\sessions-<host>\local.toml"`, i.e.
///      `C:\tmp\.config\sot\...`).
#[cfg(windows)]
fn legacy_windows_config_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        out.push(PathBuf::from(v).join("sot"));
    }
    if let Some(v) = std::env::var_os("HOME") {
        out.push(PathBuf::from(v).join(".config").join("sot"));
    }
    if let Some(v) = std::env::var_os("USERPROFILE") {
        out.push(PathBuf::from(v).join(".config").join("sot"));
    }
    // `config_dir()`'s own literal fallback, byte-for-byte — Windows roots
    // a leading separator with no drive prefix onto the current drive.
    out.push(PathBuf::from("/tmp/.config").join("sot"));
    // Built by string formatting, not `PathBuf::join` — `%SystemDrive%`'s
    // value is a bare `C:` with no trailing separator, and joining onto a
    // prefix-only `PathBuf` with no root component produces a
    // DRIVE-RELATIVE path (`C:tmp\...`, relative to that drive's current
    // dir) rather than the absolute `C:\tmp\...` intended here.
    let system_drive = std::env::var_os("SystemDrive")
        .and_then(|v| v.into_string().ok())
        .unwrap_or_else(|| "C:".to_string());
    out.push(PathBuf::from(format!("{system_drive}\\tmp\\.config\\sot")));

    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// Windows-only, one-time migration, run at boot before `scan_disk` reads
/// the registry: before this fix, `app_config_dir()` fell straight through
/// to `config_dir()` above with no Windows branch, so the registry root
/// depended on which shell launched the daemon (see `app_config_dir`'s doc
/// for the field report this fixes) — see `legacy_windows_config_candidates`
/// for every root that could have produced.
///
/// Moves ONLY the backend's own registry children
/// (`backend_registry_children`) from the FIRST candidate that has any,
/// via one `rename` per child — never the whole legacy dir, which also
/// holds frontend-owned files on Windows. For every OTHER candidate that
/// also has backend children: each `.toml` whose file name doesn't already
/// exist at the destination is COPIED in (not moved); a name that DOES
/// collide is left at the source and only warned about (naming both
/// paths) — no merge, no overwrite, no deletion anywhere in this function.
///
/// "Already migrated" is decided by the PRESENCE of backend children under
/// the new root, not by the root existing — `app_config_dir()`'s directory
/// can already exist empty (default-workspace persistence creates it) on a
/// box with nothing to migrate, and treating that as "done" would silently
/// skip real legacy data sitting in a candidate.
///
/// A failed rename of a primary child is a BOOT ERROR (`Err`, naming both
/// paths) rather than a warning: with the data only partially moved, this
/// must refuse to boot rather than silently read whatever now sits at the
/// new root (empty or partial) as if it were the whole registry — no
/// fallback to reading the old location, no seeding a fresh registry
/// beside a stranded one. Secondary-candidate copy failures stay
/// warn-only: that candidate's data is untouched (still at its original
/// path) either way, so there's nothing to strand.
///
/// Logs one INFO line per probed candidate (`legacy registry probe: <path>
/// — found (migrating)` / `— empty` / `— absent`) plus one summary line
/// when the new root already had backend children ("migration not
/// needed") — before this, a field boot log with nothing to migrate was
/// silent here, so it couldn't be told apart from "this code never ran"
/// (v0.6.0-rc.4 field report). Returns the per-candidate outcomes so tests
/// can assert on what was probed without a tracing test-capture harness
/// (this crate has none); behaviour is otherwise unchanged.
#[cfg(windows)]
fn migrate_legacy_windows_config_dir() -> Result<Vec<(PathBuf, ProbeOutcome)>> {
    let new_root = app_config_dir();
    if !backend_registry_children(&new_root).is_empty() {
        tracing::info!(root = %new_root.display(),
            "legacy registry migration not needed: new root already has backend children");
        return Ok(Vec::new());
    }
    let candidates = legacy_windows_config_candidates();
    let mut primary_done = false;
    let mut probes = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let (children, outcome) = probe_candidate(candidate);
        match outcome {
            ProbeOutcome::Found => {
                tracing::info!("legacy registry probe: {} — found (migrating)", candidate.display())
            }
            ProbeOutcome::Empty => {
                tracing::info!("legacy registry probe: {} — empty", candidate.display())
            }
            ProbeOutcome::Absent => {
                tracing::info!("legacy registry probe: {} — absent", candidate.display())
            }
        }
        probes.push((candidate.clone(), outcome));
        if children.is_empty() {
            continue;
        }
        if !primary_done {
            std::fs::create_dir_all(&new_root).with_context(|| {
                format!("create Windows config root {}", new_root.display())
            })?;
            for child in &children {
                let src = candidate.join(child);
                let dst = new_root.join(child);
                std::fs::rename(&src, &dst).with_context(|| {
                    format!(
                        "migrate legacy Windows config dir: moving {} to {} failed \
                         (registry left split across both paths)",
                        src.display(),
                        dst.display()
                    )
                })?;
                tracing::info!(from = %src.display(), to = %dst.display(),
                    "migrated legacy Windows registry child");
            }
            primary_done = true;
        } else {
            merge_secondary_legacy_windows_children(candidate, &children, &new_root);
        }
    }
    Ok(probes)
}

/// Outcome of probing one `legacy_windows_config_candidates()` entry —
/// logged and returned by `migrate_legacy_windows_config_dir` so a boot
/// with nothing to migrate is distinguishable, in the log and in tests,
/// from this code never having run.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeOutcome {
    /// The candidate directory exists and has backend registry children.
    Found,
    /// The candidate directory exists but has no backend registry children.
    Empty,
    /// The candidate directory doesn't exist.
    Absent,
}

#[cfg(windows)]
fn probe_candidate(candidate: &Path) -> (Vec<String>, ProbeOutcome) {
    if !candidate.is_dir() {
        return (Vec::new(), ProbeOutcome::Absent);
    }
    let children = backend_registry_children(candidate);
    let outcome = if children.is_empty() { ProbeOutcome::Empty } else { ProbeOutcome::Found };
    (children, outcome)
}

/// Best-effort merge for a legacy Windows config root OTHER than the one
/// adopted as primary (see `migrate_legacy_windows_config_dir`): for each
/// backend child directory it also has, copy in any `.toml` whose file
/// name isn't already present at the destination; warn (naming both
/// paths) and skip any that is. Never touches or removes the source —
/// this candidate is left exactly as it was found either way.
#[cfg(windows)]
fn merge_secondary_legacy_windows_children(candidate: &Path, children: &[String], new_root: &Path) {
    for child in children {
        let src_dir = candidate.join(child);
        let dst_dir = new_root.join(child);
        if let Err(e) = std::fs::create_dir_all(&dst_dir) {
            tracing::warn!(error = %e, dir = %dst_dir.display(),
                "could not create dir to merge a secondary legacy Windows registry child into — skipping");
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&src_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            let Some(name) = path.file_name() else { continue };
            let dst_file = dst_dir.join(name);
            if dst_file.exists() {
                tracing::warn!(from = %path.display(), to = %dst_file.display(),
                    "a second legacy Windows registry has a toml with the same name as one \
                     already migrated — left in place, not merged (rename by hand to adopt it)");
                continue;
            }
            if let Err(e) = std::fs::copy(&path, &dst_file) {
                tracing::warn!(error = %e, from = %path.display(), to = %dst_file.display(),
                    "could not copy a non-colliding toml from a secondary legacy Windows registry");
            }
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Hand-rolled scalar `key = "value"` parser scoped to *top-level*
/// (everything before the first `[section]`). Section bodies are
/// ignored so a section key with the same name as a canonical key
/// can't be mistaken for one. Numeric values (created, started) come
/// through as bare digits and are returned as the raw string.
fn parse_kv(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            // Hit the first section — stop. The frontend's persisted
            // sections live below, and we don't want their keys to leak
            // into top-level resolution.
            break;
        }
        let Some((k, v)) = t.split_once('=') else { continue };
        out.insert(k.trim().to_string(), toml_unquote(strip_quotes(v.trim())));
    }
    out
}

/// Like `parse_kv` but scoped to a `[section]` block.
fn parse_section(text: &str, section: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut in_section = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            let name = &t[1..t.len() - 1];
            in_section = name == section;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((k, v)) = t.split_once('=') else { continue };
        out.insert(k.trim().to_string(), toml_unquote(strip_quotes(v.trim())));
    }
    out
}

/// Remove the canonical (top-level) `workspace_id/slug/label/project_root/
/// tmux_session/created` keys *and* the `[kernel]` section so we can
/// rewrite them. Everything else (e.g. `[nav_state]`, `[layout]`) is
/// preserved verbatim.
fn strip_canonical_top_and_kernel(text: &str) -> String {
    const TOP_KEYS: &[&str] = &[
        "workspace_id",
        "slug",
        "label",
        "project_root",
        "tmux_session",
        "created",
        "autostart_claude",
        "agent",
        "agent_name",
        "task",
        "runtime",
    ];
    let mut out = String::new();
    let mut in_top = true;
    let mut skipping_kernel = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && trimmed.contains(']') {
            in_top = false;
            skipping_kernel = trimmed.starts_with("[kernel]");
            if skipping_kernel {
                continue;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if skipping_kernel {
            continue;
        }
        if in_top {
            // Drop canonical top-level keys; preserve any others.
            if let Some((k, _)) = trimmed.split_once('=') {
                if TOP_KEYS.contains(&k.trim()) {
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Strips the surrounding `"..."` only — no unescaping. Every reader that
/// pulls a string value out of a workspace toml pairs this with
/// `toml_unquote` (its inverse escapes are `toml_quote`'s), never used
/// alone: a bare `strip_quotes` reproduced `toml_quote`'s doubled
/// backslashes verbatim on load, the bug this pairing fixes (field defect
/// 2026-09-04 — see `toml_unquote`'s own doc).
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Inverse of `toml_quote`, applied to the inside of the quotes
/// (`strip_quotes`'s output): `\\`→`\`, `\"`→`"`, `\n`, `\r`, `\t`. An
/// escape this doesn't recognize (`\U`, `\k`, a lone trailing `\`, …) is
/// kept verbatim as backslash+char rather than dropped, so a toml written
/// by an even older build that never escaped anything at all still loads
/// unchanged — this only widens what the reader accepts, never narrows it.
///
/// Field defect (2026-09-04): before this existed, `load_toml` fed
/// `strip_quotes`'s output straight through, so every saved value that
/// `toml_quote` had escaped loaded back with the escapes still literal —
/// a `project_root` containing `\` round-tripped as doubled backslashes
/// (harmless on Windows, which tolerates repeated separators, so this hid
/// for months) and a saved Windows verbatim root (`\\?\C:\...`, doubled by
/// the writer to `\\\\?\\C:\\...`) never matched `paths::simplify_verbatim`
/// at all, so `CreateProcess` rejected it as a working directory
/// (`capsule supervisor spawn failed: The directory name is invalid. (os
/// error 267)`). `simplify_verbatim` also grew a second, single-backslash
/// prefix form to match: pass this function's OWN output through the
/// unescaper once (a raw, never-escaped legacy write's leading `\\`
/// reads as one escaped backslash, "halving" `\\?\` to `\?\`) and you can
/// see why both shapes are real on-disk data now, not just one.
fn toml_unquote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_from_label_uses_slug_and_tmux_convention() {
        let ws = Workspace::from_label("MyPkg.jl", PathBuf::from("/home/u/MyPkg.jl"), false, "none".into(), String::new(), String::new());
        assert_eq!(ws.slug, "mypkg_jl");
        assert_eq!(ws.tmux_session, "sot-be-mypkg_jl");
        assert_eq!(ws.label, "MyPkg.jl");
        assert_eq!(ws.project_root, PathBuf::from("/home/u/MyPkg.jl"));
    }

    #[test]
    fn registry_insert_and_resolve_by_id_and_slug() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        assert!(reg.resolve(Some(&id)).is_some());
        assert_eq!(reg.resolve(Some("alpha")).unwrap().slug, "alpha");
    }

    #[test]
    fn registry_default_resolves_when_id_missing() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        reg.set_default(&id);
        assert_eq!(reg.resolve(None).unwrap().slug, "alpha");
    }

    #[test]
    fn registry_resolve_unknown_is_none() {
        let reg = Workspaces::new();
        assert!(reg.resolve(Some("nope")).is_none());
        assert!(reg.resolve(None).is_none());
    }

    #[test]
    fn registry_default_id_round_trip() {
        let reg = Workspaces::new();
        assert!(reg.default_id().is_none());
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        reg.set_default(&id);
        assert_eq!(reg.default_id().as_deref(), Some(id.as_str()));
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
    fn registry_reinsert_preserves_workspace_id() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        let original_id = ws.workspace_id.clone();
        reg.insert(ws);
        // Same slug, different label — id should stay the same.
        let again = Workspace::from_label("alpha", PathBuf::from("/p/alpha-renamed"), false, "none".into(), String::new(), String::new());
        reg.insert(again);
        let resolved = reg.resolve(Some("alpha")).unwrap();
        assert_eq!(resolved.workspace_id, original_id);
        assert_eq!(resolved.project_root, PathBuf::from("/p/alpha-renamed"));
    }

    /// ADR 0042 slice L1a: `insert`'s own doc says the id-preserving
    /// re-insert (same slug) takes "the REST of the metadata" from the
    /// NEW `ws` — `runtime` must be no exception. Before `insert`'s own
    /// fix, the reconstruction branch called `Workspace::meta_only`
    /// (which always defaults `runtime` to "tmux" internally) without
    /// threading the incoming `ws.runtime` through at all, so EVERY
    /// same-slug reinsert silently reset it to "tmux" regardless of what
    /// the caller passed — exactly the "id-preserving refresh" a second
    /// `workspace.create` for an existing capsule workspace's slug is
    /// (`handlers.rs`'s own duplicate-root gate comment names this case),
    /// which still sets `runtime = "capsule"` on every call.
    #[test]
    fn registry_reinsert_takes_the_new_runtime_not_metas_default() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        reg.insert(ws);
        let mut again = Workspace::from_label("alpha", PathBuf::from("/p/alpha-renamed"), false, "none".into(), String::new(), String::new());
        again.runtime = "capsule".to_string();
        reg.insert(again);
        let resolved = reg.resolve(Some("alpha")).unwrap();
        assert_eq!(resolved.runtime, "capsule");
    }

    #[test]
    fn parse_kv_top_level_only() {
        let text = r#"
workspace_id = "x"
slug         = "alpha"
[kernel]
status = "stopped"
"#;
        let kv = parse_kv(text);
        assert_eq!(kv.get("workspace_id").map(String::as_str), Some("x"));
        assert!(kv.get("status").is_none()); // inside [kernel], not top
    }

    #[test]
    fn parse_section_scoped() {
        let text = r#"
workspace_id = "x"

[backend]
session_id = "y"
label = "MyPkg"
project_dir = "/p"
"#;
        let b = parse_section(text, "backend");
        assert_eq!(b.get("session_id").map(String::as_str), Some("y"));
        assert_eq!(b.get("label").map(String::as_str), Some("MyPkg"));
        assert!(b.get("workspace_id").is_none());
    }

    #[test]
    fn load_toml_canonical() {
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-test-canonical-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("alpha.toml");
        std::fs::write(
            &p,
            r#"
workspace_id = "ws-alpha-1"
slug         = "alpha"
label        = "Alpha.jl"
project_root = "/home/u/Alpha.jl"
tmux_session = "sot-be-alpha"
created      = 1700000000
"#,
        )
        .unwrap();
        let ws = load_toml(&p, false).unwrap().unwrap();
        assert_eq!(ws.workspace_id, "ws-alpha-1");
        assert_eq!(ws.slug, "alpha");
        assert_eq!(ws.label, "Alpha.jl");
        assert_eq!(ws.project_root, PathBuf::from("/home/u/Alpha.jl"));
        assert_eq!(ws.tmux_session, "sot-be-alpha");
        // ADR 0042 slice L1a: a toml predating the `runtime` key defaults
        // to this platform's ordinary workspace runtime — "tmux",
        // byte-for-byte today's Unix behaviour; "capsule" on Windows
        // (Codex review, PR #175 — see `load_toml`'s own comment: tmux
        // never runs on Windows at all).
        #[cfg(not(windows))]
        assert_eq!(ws.runtime, "tmux");
        #[cfg(windows)]
        assert_eq!(ws.runtime, "capsule");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_toml_canonical_round_trips_capsule_runtime() {
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-test-capsule-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("beta.toml");
        std::fs::write(
            &p,
            r#"
workspace_id = "ws-beta-1"
slug         = "beta"
label        = "Beta.jl"
project_root = "/home/u/Beta.jl"
tmux_session = "sot-be-beta"
created      = 1700000000
runtime      = "capsule"
"#,
        )
        .unwrap();
        let ws = load_toml(&p, false).unwrap().unwrap();
        assert_eq!(ws.runtime, "capsule");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Field defect (2026-09-04): a toml written by a pre-fix build can
    /// carry the Windows extended-length (verbatim) form `std::fs::
    /// canonicalize` returns there (`\\?\C:\...`); `CreateProcess` rejects
    /// that as a working directory. `load_toml` must hand back the plain
    /// form regardless of what's on disk. Windows-only: `simplify_verbatim`
    /// is a no-op on every other platform, so this toml would (correctly)
    /// load unchanged there.
    ///
    /// This file's `project_root` line is raw/unescaped — the shape a
    /// build that never escaped anything at all would have written. Once
    /// `toml_unquote` runs (below `load_toml_canonical`'s escaped-writer
    /// sibling `_unescapes_writer_escaped_verbatim_prefix`), its leading
    /// `\\` reads as one escaped backslash and the prefix halves to
    /// `\?\` — exercising `simplify_verbatim`'s single-backslash branch,
    /// not its original double-backslash one.
    #[test]
    #[cfg(windows)]
    fn load_toml_canonical_strips_windows_verbatim_prefix() {
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-test-verbatim-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("gamma.toml");
        std::fs::write(
            &p,
            r#"
workspace_id = "ws-gamma-1"
slug         = "gamma"
label        = "Gamma.jl"
project_root = "\\?\C:\Users\u\.julia\dev\Gamma.jl"
tmux_session = "sot-be-gamma"
created      = 1700000000
"#,
        )
        .unwrap();
        let ws = load_toml(&p, false).unwrap().unwrap();
        assert_eq!(
            ws.project_root,
            PathBuf::from(r"C:\Users\u\.julia\dev\Gamma.jl")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The realistic on-disk shape after this fix: the real writer
    /// (`toml_quote`) escapes every backslash, so a pre-fix-build `save()`
    /// of a verbatim root doubles them — `\\?\C:\...` becomes
    /// `\\\\?\\C:\\...` in the file. `load_toml` must unescape
    /// (`toml_unquote`) before `simplify_verbatim` ever sees it, which
    /// restores the true `\\?\` prefix and strips it via
    /// `simplify_verbatim`'s original double-backslash branch — the
    /// sibling test above covers the OTHER on-disk shape (a raw,
    /// never-escaped write, which lands on the new single-backslash
    /// branch instead).
    #[test]
    #[cfg(windows)]
    fn load_toml_canonical_unescapes_writer_escaped_verbatim_prefix() {
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-test-verbatim-escaped-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("delta.toml");
        std::fs::write(
            &p,
            r#"
workspace_id = "ws-delta-1"
slug         = "delta"
label        = "Delta.jl"
project_root = "\\\\?\\C:\\Users\\u\\HomeLab\\x"
tmux_session = "sot-be-delta"
created      = 1700000000
"#,
        )
        .unwrap();
        let ws = load_toml(&p, false).unwrap().unwrap();
        assert_eq!(ws.project_root, PathBuf::from(r"C:\Users\u\HomeLab\x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_toml_legacy_backend_block() {
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-test-legacy-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("legacy.toml");
        std::fs::write(
            &p,
            r#"
[backend]
session_id   = "sess-old"
label        = "LegacyPkg.jl"
project_dir  = "/home/u/LegacyPkg.jl"
tmux_session = "sot-be-legacypkg.jl"
started      = 1700000000
pid          = 12345
"#,
        )
        .unwrap();
        let ws = load_toml(&p, true).unwrap().unwrap();
        assert_eq!(ws.workspace_id, "sess-old");
        assert_eq!(ws.slug, "legacypkg_jl");
        assert_eq!(ws.label, "LegacyPkg.jl");
        assert_eq!(ws.project_root, PathBuf::from("/home/u/LegacyPkg.jl"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_toml_legacy_rejected_when_legacy_off() {
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-test-legacy-off-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("only-backend.toml");
        std::fs::write(&p, "[backend]\nlabel = \"x\"\nproject_dir = \"/p\"\n").unwrap();
        let result = load_toml(&p, false).unwrap();
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_canonical_keeps_other_sections() {
        let input = r#"workspace_id = "old"
slug         = "alpha"
label        = "Alpha"
project_root = "/p"
tmux_session = "sot-be-alpha"
created      = 1700000000

[kernel]
status = "stopped"

[nav_state]
mode = "files"
cursor_path = "src/lib.jl"
"#;
        let stripped = strip_canonical_top_and_kernel(input);
        assert!(!stripped.contains("workspace_id"));
        assert!(!stripped.contains("[kernel]"));
        assert!(!stripped.contains("status = \"stopped\""));
        assert!(stripped.contains("[nav_state]"));
        assert!(stripped.contains("cursor_path = \"src/lib.jl\""));
    }

    // `app_config_dir()`'s platform dispatch, and the one-time Windows
    // migration off the old HOME-derived root. Serialized under the
    // crate-wide `paths::ENV_TEST_LOCK` (Codex review, PR #175: a
    // module-local mutex here couldn't stop a test in THIS module from
    // racing a `paths.rs`/`session_state.rs` test over the same env vars).

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        xdg_config_home: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
        localappdata: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
        system_drive: Option<std::ffi::OsString>,
        // Snapshotted/restored too (not just set) because a few tests below
        // pin it to a known value so `migrate_legacy_state_dirs`'s
        // `state_host()` calls resolve deterministically — the real
        // hostname would otherwise leak into the "-<host>" suffix these
        // tests assert on, and a leaked value would poison every other
        // `state_host()`-reading test sharing this crate-wide lock.
        sot_state_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("XDG_CONFIG_HOME", &self.xdg_config_home),
                ("HOME", &self.home),
                ("LOCALAPPDATA", &self.localappdata),
                ("USERPROFILE", &self.userprofile),
                ("SystemDrive", &self.system_drive),
                ("SOT_STATE_HOST", &self.sot_state_host),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn env_guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            home: std::env::var_os("HOME"),
            localappdata: std::env::var_os("LOCALAPPDATA"),
            userprofile: std::env::var_os("USERPROFILE"),
            system_drive: std::env::var_os("SystemDrive"),
            sot_state_host: std::env::var_os("SOT_STATE_HOST"),
            _serial: serial,
        }
    }

    /// Field defect (2026-09-04) root-cause test: the writer (`toml_quote`)
    /// escapes every backslash, but the pre-fix reader (`strip_quotes`
    /// alone) never undid that, so a `project_root` containing `\` came
    /// back with every backslash DOUBLED — a no-op disguise on Windows,
    /// which tolerates repeated separators, but not an identity round
    /// trip. Runs on every OS: this is a `toml_quote`/`toml_unquote`
    /// symmetry bug, independent of `simplify_verbatim` (which is a
    /// no-op here — the path below isn't verbatim-prefixed).
    #[test]
    fn save_load_round_trips_backslash_project_root() {
        let _guard = env_guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-test-roundtrip-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var("LOCALAPPDATA", &dir);
        std::env::remove_var("USERPROFILE");
        std::env::set_var("SOT_STATE_HOST", "roundtrip-test");

        let ws = Workspace::meta_only(
            "ws-rt-1".to_string(),
            "rt-backslash".to_string(),
            "RoundTrip.jl".to_string(),
            PathBuf::from(r"C:\Users\u\HomeLab\x"),
            "sot-be-rt-backslash".to_string(),
            1700000000,
            false,
            "none".to_string(),
            String::new(),
            String::new(),
        );
        let toml_path = save(&ws).unwrap();
        let loaded = load_toml(&toml_path, false).unwrap().unwrap();
        assert_eq!(
            loaded.project_root, ws.project_root,
            "project_root must round-trip through save()/load_toml() identically"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same round trip, covering `toml_quote`'s other escapes — an
    /// embedded `"` and a newline — via `agent_name`/`task`, the two
    /// free-text fields that share its quoting (see `save()`'s comment
    /// above the `agent_name`/`task` lines).
    #[test]
    fn save_load_round_trips_quotes_and_newlines_in_free_text_fields() {
        let _guard = env_guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-test-roundtrip-quotes-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var("LOCALAPPDATA", &dir);
        std::env::remove_var("USERPROFILE");
        std::env::set_var("SOT_STATE_HOST", "roundtrip-test");

        let ws = Workspace::meta_only(
            "ws-rt-2".to_string(),
            "rt-quotes".to_string(),
            "RoundTrip2.jl".to_string(),
            PathBuf::from("/home/u/RoundTrip2.jl"),
            "sot-be-rt-quotes".to_string(),
            1700000000,
            false,
            "claude".to_string(),
            "peer-\"nick\"".to_string(),
            "line one\nline two".to_string(),
        );
        let toml_path = save(&ws).unwrap();
        let loaded = load_toml(&toml_path, false).unwrap().unwrap();
        assert_eq!(loaded.agent_name, ws.agent_name);
        assert_eq!(loaded.task, ws.task);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(windows))]
    fn app_config_dir_unix_still_prefers_xdg_config_home() {
        let _guard = env_guarded();
        std::env::set_var("XDG_CONFIG_HOME", "/xdg-config");
        std::env::set_var("HOME", "/home/someone");
        assert_eq!(app_config_dir(), PathBuf::from("/xdg-config/sot"));
    }

    #[test]
    #[cfg(windows)]
    fn app_config_dir_windows_uses_localappdata_config_subdir() {
        let _guard = env_guarded();
        std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
        assert_eq!(
            app_config_dir(),
            PathBuf::from(r"C:\Users\someone\AppData\Local\sot\config")
        );
    }

    #[test]
    #[cfg(windows)]
    fn app_config_dir_windows_ignores_xdg_config_home() {
        let _guard = env_guarded();
        std::env::set_var("XDG_CONFIG_HOME", r"C:\should\be\ignored");
        std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
        assert_eq!(
            app_config_dir(),
            PathBuf::from(r"C:\Users\someone\AppData\Local\sot\config")
        );
    }

    #[test]
    #[cfg(windows)]
    fn app_config_dir_windows_falls_back_to_userprofile_when_localappdata_unset() {
        let _guard = env_guarded();
        std::env::remove_var("LOCALAPPDATA");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("USERPROFILE", r"C:\Users\someone");
        assert_eq!(
            app_config_dir(),
            PathBuf::from(r"C:\Users\someone\AppData\Local\sot\config")
        );
    }

    #[test]
    #[cfg(windows)]
    #[should_panic(expected = "cannot resolve the Windows state root")]
    fn app_config_dir_windows_panics_when_localappdata_and_userprofile_are_both_unset() {
        let _guard = env_guarded();
        std::env::remove_var("LOCALAPPDATA");
        std::env::remove_var("USERPROFILE");
        let _ = app_config_dir();
    }

    /// Field incident (2026-09-03): a Windows default row's on-disk toml
    /// read `runtime = "tmux"` — some earlier writer's leftover, from
    /// before this row had ever been through the current boot-seed logic
    /// — and the OLD preserve-verbatim behaviour carried that forward on
    /// every boot with no self-healing: the daemon then refused to start
    /// the row at all (tmux is refused outright on Windows, #177) and it
    /// couldn't be destroyed either (`default_workspace_not_destroyable`)
    /// — a dead end. Exercises the REAL on-disk shape through
    /// `scan_disk`/`load_toml` (not a hand-built `Workspace`) — proving
    /// the toml really does read back "tmux" — then proves
    /// `default_row_runtime` (the function `server::run`'s boot seed now
    /// routes through) corrects it to "capsule".
    #[test]
    #[cfg(windows)]
    fn default_row_runtime_corrects_a_stale_on_disk_tmux_value() {
        let _guard = env_guarded();
        let base = std::env::temp_dir().join(format!(
            "sot-ws-test-default-row-capsule-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("LOCALAPPDATA", &base);
        std::env::remove_var("XDG_CONFIG_HOME");

        let dir = workspaces_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sot.toml"),
            r#"
workspace_id  = "ws-sot-1"
slug          = "sot"
label         = ".SoT"
project_root  = "/home/u/ship-of-tools"
tmux_session  = "sot-be-sot"
created       = 1700000000
autostart_claude = false
agent         = "none"
runtime       = "tmux"
"#,
        )
        .unwrap();

        let reg = Workspaces::new();
        scan_disk(&reg).unwrap();
        let existing = reg.resolve(Some("sot")).unwrap();
        // Prove this really exercises the on-disk "tmux" value, not a
        // tautology — `load_toml` read it back unmodified.
        assert_eq!(existing.runtime, "tmux");

        let corrected = default_row_runtime(Some(&existing.runtime), "tmux");
        assert_eq!(corrected, "capsule");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 2026-09-04 amendment: a genuinely first-ever launch seeds the
    /// default/home row as an INERT ANCHOR — no agent, no autostart — on
    /// EVERY host, not just non-Windows. Runs (and must pass) on every
    /// platform: unlike the corrupted-row case below, this arm no longer
    /// branches on `cfg!(windows)` at all.
    #[test]
    fn default_row_launch_seed_first_launch_is_inert_everywhere() {
        assert_eq!(
            default_row_launch_seed(None),
            (false, "none".to_string(), String::new(), String::new())
        );
    }

    /// An existing default row whose on-disk `runtime` is `"capsule"` —
    /// healthy on every platform this function runs on (Windows requires
    /// it; every other host merely permits it) — survives verbatim: its
    /// launch fields are never silently clobbered back to the inert
    /// defaults just because it was re-registered at boot.
    #[test]
    fn default_row_launch_seed_preserves_a_healthy_existing_row() {
        assert_eq!(
            default_row_launch_seed(Some(("capsule", true, "claude", "kal-sot", "hello"))),
            (
                true,
                "claude".to_string(),
                "kal-sot".to_string(),
                "hello".to_string()
            )
        );
    }

    /// Non-Windows only: `runtime` values other than `"capsule"` (e.g.
    /// the ordinary `"tmux"` every non-Windows row actually carries) are
    /// never "corrupted" off Windows — the corrupted-row re-seed is a
    /// Windows-only concept (`default_row_runtime`'s own doc: tmux is
    /// refused outright on Windows, #177, so only THERE is a non-capsule
    /// runtime necessarily a leftover). Existing launch fields still
    /// survive verbatim.
    #[test]
    #[cfg(not(windows))]
    fn default_row_launch_seed_preserves_a_tmux_row_off_windows() {
        assert_eq!(
            default_row_launch_seed(Some(("tmux", true, "claude", "kal-sot", "hello"))),
            (
                true,
                "claude".to_string(),
                "kal-sot".to_string(),
                "hello".to_string()
            )
        );
    }

    /// Windows only (#185's corrupted-row incident): an on-disk `runtime`
    /// other than `"capsule"` means whatever wrote it also flipped
    /// `agent`/`autostart_claude` — preserving those verbatim would boot
    /// a capsule with a corrupted agent, so this row re-seeds to the same
    /// inert defaults a first-ever launch gets, same as
    /// `default_row_runtime` re-seeds its runtime to `"capsule"` for it.
    #[test]
    #[cfg(windows)]
    fn default_row_launch_seed_reseeds_a_corrupted_windows_row_to_inert() {
        assert_eq!(
            default_row_launch_seed(Some(("tmux", true, "claude", "kal-sot", "hello"))),
            (false, "none".to_string(), String::new(), String::new())
        );
    }

    /// Scratch roots for one migration test: `USERPROFILE`-, `SystemDrive`-
    /// and `XDG_CONFIG_HOME`-style dirs, plus a `LOCALAPPDATA`-style dir
    /// for the new root. All under one per-test base so a single
    /// `remove_dir_all` on the base cleans everything up.
    #[cfg(windows)]
    struct MigrationScratch {
        base: PathBuf,
        userprofile: PathBuf,
        system_drive: String,
        xdg_config_home: PathBuf,
        localappdata: PathBuf,
    }

    #[cfg(windows)]
    impl MigrationScratch {
        fn new(name: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let base = std::env::temp_dir().join(format!(
                "sot-ws-win-config-migrate-{}-{}-{name}",
                std::process::id(),
                n
            ));
            Self {
                userprofile: base.join("userprofile"),
                // No trailing separator, matching a real `%SystemDrive%`
                // value (`C:`) — the base dir's string form stands in for
                // the drive letter here.
                system_drive: base.join("sysdrive-root").to_string_lossy().into_owned(),
                xdg_config_home: base.join("xdg-config-home"),
                localappdata: base.join("localappdata"),
                base,
            }
        }

        fn userprofile_legacy(&self) -> PathBuf {
            self.userprofile.join(".config").join("sot")
        }

        fn system_drive_legacy(&self) -> PathBuf {
            PathBuf::from(format!("{}\\tmp\\.config\\sot", self.system_drive))
        }

        fn xdg_config_home_legacy(&self) -> PathBuf {
            self.xdg_config_home.join("sot")
        }

        fn new_root(&self) -> PathBuf {
            self.localappdata.join("sot").join("config")
        }

        /// Clears every candidate-relevant var, then sets only
        /// `USERPROFILE`/`SystemDrive`/`LOCALAPPDATA` (what a real Windows
        /// login always has) — a test that wants `XDG_CONFIG_HOME` or
        /// `HOME` in the mix sets it after calling this.
        fn apply_env(&self) {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("HOME");
            std::env::set_var("USERPROFILE", &self.userprofile);
            std::env::set_var("SystemDrive", &self.system_drive);
            std::env::set_var("LOCALAPPDATA", &self.localappdata);
        }
    }

    #[cfg(windows)]
    impl Drop for MigrationScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    #[cfg(windows)]
    fn backend_registry_children_finds_workspace_and_session_dirs_not_frontend_files() {
        let s = MigrationScratch::new("children-scan");
        let root = s.base.join("scan-root");
        std::fs::create_dir_all(root.join("workspaces-host")).unwrap();
        std::fs::create_dir_all(root.join("sessions-otherhost")).unwrap();
        std::fs::create_dir_all(root.join("workspaces")).unwrap();
        std::fs::write(root.join("settings.toml"), "").unwrap();
        std::fs::write(root.join("hosts.toml"), "").unwrap();
        assert_eq!(
            backend_registry_children(&root),
            vec!["sessions-otherhost", "workspaces", "workspaces-host"]
        );
    }

    #[test]
    #[cfg(windows)]
    fn migrate_moves_only_backend_children_and_leaves_frontend_files_in_place() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("frontend-untouched");
        let legacy = s.userprofile_legacy();
        let workspace_toml = legacy.join("workspaces-host").join("alpha.toml");
        std::fs::create_dir_all(workspace_toml.parent().unwrap()).unwrap();
        std::fs::write(&workspace_toml, "slug = \"alpha\"\n").unwrap();
        // Frontend-owned files sharing the same legacy root — must survive.
        std::fs::write(legacy.join("settings.toml"), "frontend settings").unwrap();
        std::fs::write(legacy.join("hosts.toml"), "frontend hosts").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!legacy.join("workspaces-host").exists(), "backend child should have moved");
        assert!(s.new_root().join("workspaces-host").join("alpha.toml").is_file());
        // Frontend files are untouched at their original path — the
        // legacy dir itself was never renamed, only its backend children.
        assert_eq!(
            std::fs::read_to_string(legacy.join("settings.toml")).unwrap(),
            "frontend settings"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("hosts.toml")).unwrap(),
            "frontend hosts"
        );
    }

    #[test]
    #[cfg(windows)]
    fn migrate_prefers_xdg_config_home_over_every_other_candidate() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("xdg-first");
        s.apply_env();
        std::env::set_var("XDG_CONFIG_HOME", &s.xdg_config_home);
        let xdg_legacy = s.xdg_config_home_legacy();
        std::fs::create_dir_all(xdg_legacy.join("sessions-host")).unwrap();
        // A USERPROFILE-rooted candidate also exists, but XDG_CONFIG_HOME
        // must win since it's probed first.
        std::fs::create_dir_all(s.userprofile_legacy().join("sessions-host")).unwrap();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!xdg_legacy.join("sessions-host").exists());
        assert!(s.new_root().join("sessions-host").is_dir());
        // The USERPROFILE candidate was only SECONDARY here — its own
        // empty child dir is left exactly as found, not deleted.
        assert!(s.userprofile_legacy().join("sessions-host").is_dir());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_falls_back_to_system_drive_root_when_nothing_earlier_has_children() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("sysdrive");
        let legacy = s.system_drive_legacy();
        std::fs::create_dir_all(legacy.join("sessions-host")).unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!legacy.join("sessions-host").exists());
        assert!(s.new_root().join("sessions-host").is_dir());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_copies_noncolliding_tomls_from_a_secondary_candidate_and_warns_on_collision() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("secondary-merge");
        let primary = s.userprofile_legacy();
        std::fs::create_dir_all(primary.join("workspaces-host")).unwrap();
        std::fs::write(primary.join("workspaces-host").join("alpha.toml"), "primary alpha").unwrap();
        let secondary = s.system_drive_legacy();
        std::fs::create_dir_all(secondary.join("workspaces-host")).unwrap();
        // Non-colliding: gets copied in.
        std::fs::write(secondary.join("workspaces-host").join("beta.toml"), "secondary beta").unwrap();
        // Colliding name: left at the source, not overwritten at the dest.
        std::fs::write(secondary.join("workspaces-host").join("alpha.toml"), "secondary alpha").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        let dst = s.new_root().join("workspaces-host");
        assert_eq!(std::fs::read_to_string(dst.join("alpha.toml")).unwrap(), "primary alpha");
        assert_eq!(std::fs::read_to_string(dst.join("beta.toml")).unwrap(), "secondary beta");
        // Secondary candidate is left in place entirely — including the
        // colliding file, which was never deleted or overwritten.
        assert_eq!(
            std::fs::read_to_string(secondary.join("workspaces-host").join("alpha.toml")).unwrap(),
            "secondary alpha"
        );
    }

    #[test]
    #[cfg(windows)]
    fn migrate_no_op_when_new_root_already_has_backend_children() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("already-migrated");
        let legacy = s.userprofile_legacy();
        std::fs::create_dir_all(legacy.join("workspaces-host")).unwrap();
        std::fs::write(legacy.join("workspaces-host").join("stray.toml"), "should not move").unwrap();
        std::fs::create_dir_all(s.new_root().join("workspaces-host")).unwrap();
        std::fs::write(s.new_root().join("workspaces-host").join("canonical.toml"), "canonical").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(legacy.join("workspaces-host").join("stray.toml").is_file());
        assert!(s.new_root().join("workspaces-host").join("canonical.toml").is_file());
        assert!(!s.new_root().join("workspaces-host").join("stray.toml").exists());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_proceeds_when_new_root_exists_but_has_no_backend_children_yet() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("empty-new-root");
        let legacy = s.userprofile_legacy();
        std::fs::create_dir_all(legacy.join("workspaces-host")).unwrap();
        std::fs::write(legacy.join("workspaces-host").join("alpha.toml"), "alpha").unwrap();
        // The new root dir already exists (e.g. default-workspace
        // persistence created it) but holds no backend children yet — this
        // must NOT be mistaken for "already migrated".
        std::fs::create_dir_all(s.new_root()).unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!legacy.join("workspaces-host").exists());
        assert!(s.new_root().join("workspaces-host").join("alpha.toml").is_file());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_no_op_when_no_candidate_has_backend_children() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("no-legacy");
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!s.new_root().exists());
    }

    /// The daemon logs one `tracing::info!` line per probed candidate
    /// (found/empty/absent) so a field boot log can tell "probed, nothing
    /// there" apart from "this code never ran" — but this crate has no
    /// tracing test-capture harness, so this asserts on the return value
    /// (`Vec<(PathBuf, ProbeOutcome)>`) the same probe loop reports
    /// through, rather than on captured log output.
    #[test]
    #[cfg(windows)]
    fn migrate_returns_probe_outcome_per_candidate_including_the_empty_case() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("probe-outcomes");
        // USERPROFILE candidate: directory exists but has no backend
        // registry children — "empty", the case a silent log can't be
        // told apart from "absent" or "never probed".
        std::fs::create_dir_all(s.userprofile_legacy()).unwrap();
        // SystemDrive candidate: has backend children — "found".
        std::fs::create_dir_all(s.system_drive_legacy().join("workspaces-host")).unwrap();
        s.apply_env();

        let probes = migrate_legacy_windows_config_dir().unwrap();

        let outcome_for = |path: &Path| {
            probes.iter().find(|(p, _)| p == path).map(|(_, o)| *o)
        };
        assert_eq!(outcome_for(&s.userprofile_legacy()), Some(ProbeOutcome::Empty));
        assert_eq!(outcome_for(&s.system_drive_legacy()), Some(ProbeOutcome::Found));
    }

    /// End-to-end field defect (2026-09-04): a PRIMARY legacy candidate
    /// already had the host-suffixed dir (an earlier boot at that root had
    /// already run the per-host migration), while a SECONDARY candidate
    /// still had an un-suffixed one holding a row the primary never saw.
    /// `migrate_legacy_windows_config_dir` moves the primary's suffixed dir
    /// straight across (name preserved) and merges the secondary's
    /// un-suffixed one in verbatim-named — landing an un-suffixed
    /// `workspaces` at the new root right next to the suffixed one.
    /// `migrate_legacy_state_dirs`, called right after (mirroring
    /// `scan_disk`'s own call order), used to see the suffixed sibling
    /// already present and stop, stranding the secondary's row where the
    /// daemon's own `workspaces_dir()` never scans. This proves both the
    /// stranding (mid-run) and the fold that now recovers it.
    #[test]
    #[cfg(windows)]
    fn migrate_then_fold_lands_every_legacy_row_in_the_host_suffixed_dir() {
        let _guard = env_guarded();
        std::env::set_var("SOT_STATE_HOST", "host");
        let s = MigrationScratch::new("migrate-then-fold");
        let primary = s.userprofile_legacy();
        std::fs::create_dir_all(primary.join("workspaces-host")).unwrap();
        std::fs::write(primary.join("workspaces-host").join("alpha.toml"), "alpha").unwrap();
        let secondary = s.system_drive_legacy();
        std::fs::create_dir_all(secondary.join("workspaces")).unwrap();
        std::fs::write(secondary.join("workspaces").join("beta.toml"), "beta").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();
        // Confirms the setup reproduces the stranding, not just the fix:
        // beta landed un-suffixed at the new root, next to workspaces-host.
        assert!(s.new_root().join("workspaces").join("beta.toml").is_file());

        migrate_legacy_state_dirs();

        let dst = s.new_root().join("workspaces-host");
        assert_eq!(
            std::fs::read_to_string(dst.join("alpha.toml")).unwrap(),
            "alpha"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("beta.toml")).unwrap(),
            "beta"
        );
        assert!(
            !s.new_root().join("workspaces").exists(),
            "the stranded un-suffixed dir must be folded in and removed"
        );
    }

    /// A box that already ran the broken migration once (a pre-fix build)
    /// has the stranding baked in from a PRIOR boot — not created fresh by
    /// THIS boot's `migrate_legacy_windows_config_dir` (that step is a
    /// no-op here: `app_config_dir()`'s backend children are already
    /// non-empty). Exercises `migrate_legacy_state_dirs` folding a
    /// pre-existing stranded dir on its own, with no windows-config-dir
    /// migration involved at all.
    #[test]
    #[cfg(windows)]
    fn fold_adopts_a_dir_stranded_by_an_earlier_broken_boot() {
        let _guard = env_guarded();
        std::env::set_var("SOT_STATE_HOST", "host");
        let s = MigrationScratch::new("fold-stranded");
        s.apply_env();
        let new_root = s.new_root();
        std::fs::create_dir_all(new_root.join("workspaces-host")).unwrap();
        std::fs::write(new_root.join("workspaces-host").join("gamma.toml"), "gamma").unwrap();
        std::fs::create_dir_all(new_root.join("workspaces")).unwrap();
        std::fs::write(new_root.join("workspaces").join("delta.toml"), "delta").unwrap();

        migrate_legacy_state_dirs();

        let dst = new_root.join("workspaces-host");
        assert_eq!(
            std::fs::read_to_string(dst.join("gamma.toml")).unwrap(),
            "gamma"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("delta.toml")).unwrap(),
            "delta"
        );
        assert!(!new_root.join("workspaces").exists());
    }

    /// Collision: the destination already has a toml with the same name.
    /// The destination row wins outright; the source file is left exactly
    /// where it was (not deleted, not overwritten) and the now-not-actually
    /// -empty legacy dir is left too, not removed — same rule
    /// `migrate_legacy_windows_config_dir`'s own secondary-candidate merge
    /// uses (`merge_secondary_legacy_windows_children`).
    #[test]
    #[cfg(windows)]
    fn fold_keeps_the_destination_row_on_a_name_collision() {
        let _guard = env_guarded();
        std::env::set_var("SOT_STATE_HOST", "host");
        let s = MigrationScratch::new("fold-collision");
        s.apply_env();
        let new_root = s.new_root();
        std::fs::create_dir_all(new_root.join("workspaces-host")).unwrap();
        std::fs::write(
            new_root.join("workspaces-host").join("alpha.toml"),
            "canonical",
        )
        .unwrap();
        std::fs::create_dir_all(new_root.join("workspaces")).unwrap();
        std::fs::write(new_root.join("workspaces").join("alpha.toml"), "stray").unwrap();

        migrate_legacy_state_dirs();

        assert_eq!(
            std::fs::read_to_string(new_root.join("workspaces-host").join("alpha.toml")).unwrap(),
            "canonical"
        );
        assert_eq!(
            std::fs::read_to_string(new_root.join("workspaces").join("alpha.toml")).unwrap(),
            "stray"
        );
        assert!(
            new_root.join("workspaces").is_dir(),
            "non-empty legacy dir must not be removed"
        );
    }
}
