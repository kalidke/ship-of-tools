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
use crate::session::Session;
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
    /// The shared preview.changed bus + session handle for per-workspace
    /// watcher spawns (2026-07-10 multiwatch). Installed once at startup via
    /// `set_watch_bus`, before workspace registration; `None` in tests.
    watch_bus: Option<(Session, broadcast::Sender<PreviewChanged>)>,
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
                Workspace::meta_only(
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
                )
            }
            None => ws,
        };
        let arc = Arc::new(final_ws);
        g.by_slug
            .insert(arc.slug.clone(), arc.workspace_id.clone());
        g.by_id.insert(arc.workspace_id.clone(), arc.clone());
        if let Some((sess, tx)) = g.watch_bus.clone() {
            Self::spawn_workspace_watcher(&arc, sess, tx);
        }
        arc
    }

    /// Spawn `ws`'s file watcher onto the shared bus. Cheap inline
    /// (`Watcher::spawn` opens the inotify fd and defers the recursive
    /// registration walk to a background thread — the NFS-stall hardening),
    /// so calling under the registry lock is fine. Failure is a warning:
    /// previews still work, that workspace just won't live-refresh.
    fn spawn_workspace_watcher(
        ws: &Arc<Workspace>,
        session: Session,
        tx: broadcast::Sender<PreviewChanged>,
    ) {
        if ws.watcher.get().is_some() {
            return; // already spawned (or recorded as failed)
        }
        let spawned = match ws.files_mode() {
            Ok(fm) => match Watcher::spawn(
                fm.root_path(),
                session,
                fm.clone(),
                tx,
                Some(ws.slug.clone()),
            ) {
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
    pub fn set_watch_bus(&self, session: Session, tx: broadcast::Sender<PreviewChanged>) {
        let existing: Vec<Arc<Workspace>> = {
            let mut g = self.inner.write().expect("workspaces lock");
            g.watch_bus = Some((session.clone(), tx.clone()));
            g.by_id.values().cloned().collect()
        };
        for ws in existing {
            Self::spawn_workspace_watcher(&ws, session.clone(), tx.clone());
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
        let project_root = PathBuf::from(kv.get("project_root").cloned().unwrap_or_default());
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
        return Ok(Some(Workspace::meta_only(
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
        )));
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
    let project_root = PathBuf::from(
        backend
            .get("project_dir")
            .cloned()
            .unwrap_or_else(|| ".".into()),
    );
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
    // \n/\r/\t). `strip_quotes` on the load side only removes the
    // surrounding quotes without unescaping, so — same as `label` —
    // an embedded `"` or newline does not round-trip perfectly. In
    // practice comm-spawn keeps `task` single-line, so this matches the
    // existing `label` limitation rather than introducing a new one.
    body.push_str(&format!("agent_name    = {}\n", toml_quote(&ws.agent_name)));
    body.push_str(&format!("task          = {}\n", toml_quote(&ws.task)));

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
fn state_host() -> String {
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
/// host's suffixed ones. Runs at daemon boot (from `load_all`); the first
/// post-deploy boot on the cohort inherits the legacy state (that's the
/// primary dev box — the only daemon that ever wrote it), every other host starts fresh, and
/// public single-home installs have nothing to migrate. Rename failures warn
/// and leave the legacy dir in place (nothing is destroyed).
pub(crate) fn migrate_legacy_state_dirs() {
    for name in ["workspaces", "sessions"] {
        let legacy = app_config_dir().join(name);
        let per_host = app_config_dir().join(format!("{name}-{}", state_host()));
        if legacy.is_dir() && !per_host.exists() {
            match std::fs::rename(&legacy, &per_host) {
                Ok(()) => tracing::info!(from = %legacy.display(), to = %per_host.display(),
                    "migrated legacy state dir to per-host (ADR 0013/0014 addendum)"),
                Err(e) => tracing::warn!(error = %e, from = %legacy.display(),
                    "legacy state dir migration failed — leaving in place"),
            }
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
pub(crate) fn app_config_dir() -> PathBuf {
    config_dir().join("sot")
}

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
        out.insert(k.trim().to_string(), strip_quotes(v.trim()).to_string());
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
        out.insert(k.trim().to_string(), strip_quotes(v.trim()).to_string());
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
}
