//! The row registry: `Workspaces`, the rows (`Workspace`) it holds and its run gate.

mod registry;
pub(crate) mod anchor;
pub(crate) mod gate;
pub(super) mod ops;
pub(crate) mod reauth;
pub(crate) mod run;
pub(crate) mod spawn;
pub(crate) mod store;
pub(crate) mod workspace;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};

use serde_json::json;
use sot_protocol::Frame;
use tokio::sync::broadcast;

use crate::files::concept::ConceptStore;
use crate::files::tree::FilesMode;
use crate::sidecars::kernel::Kernel;
use crate::paths::slug;
use crate::sidecars::repl::{Repl, ReplFrameMsg};
use crate::files::watcher::{PreviewChanged, Watcher};
use crate::server::reply::HandlerOutput;

use gate::RunGate;
use workspace::PhaseCell;

/// The row a hint names, or the reply an op sends when none is registered.
pub(crate) fn row_or_reply(
    workspaces: &Workspaces,
    hint: Option<&str>,
    req_id: u64,
    op: &str,
) -> std::result::Result<Arc<Workspace>, HandlerOutput> {
    workspaces.resolve(hint).ok_or_else(|| {
        vec![(
            Frame::res(
                req_id,
                op,
                json!({ "error": format!("unknown workspace: {hint:?}"), "code": "unknown_workspace" }),
            ),
            None,
        )]
    })
}

/// One deduplicated workspace lifecycle event. The daemon broadcasts one
/// per successful create/destroy; each connection turns it into a
/// `workspace.changed` evt frame. Mirrors `watcher::PreviewChanged`.
#[derive(Clone, Debug)]
pub struct WorkspaceChanged {
    pub action: String,
    pub slug: String,
    pub workspace_id: String,
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
    /// The row's session name, `sot-be-<slug>` (`rows::session_name`; a row
    /// loaded from a stored config keeps its stored name): the one stable
    /// token `pty.open` / `lane.connect` `target` address
    /// the row by, the same for both runtimes, and for a `"tmux"`-runtime
    /// row the real tmux session's name. Fixed for the row's lifetime —
    /// the frontend keys its per-row UI state and the Sessions tree on it.
    pub session_name: String,
    pub created: i64,
    /// Whether the frontend should launch claude on first attach to this
    /// workspace's session. Plain metadata field; persisted in the toml
    /// and defaulted to false for older tomls that lack the key.
    pub autostart_claude: bool,
    /// Which agent this workspace auto-starts: "claude" | "codex" |
    /// "none". Mutex so `reset_agent_to_none` can edit in place without
    /// discarding the row's phase cell, activation error, or observer task.
    pub(crate) agent: Mutex<String>,
    /// The sot-comm handle the spawned agent should join as. Same reasoning as `agent`.
    pub(crate) agent_name: Mutex<String>,
    /// The initial instruction the FE delivers to the spawned agent after
    /// auto-starting claude. Plain metadata; persisted in the toml and
    /// defaulted to "" when absent.
    pub task: String,
    /// ADR 0042 slice L1a: `"tmux"` | `"capsule"` — which runtime hosts
    /// this workspace's agent pane. Plain metadata; persisted in the toml
    /// and, for an older toml that lacks the key, defaulted to `"capsule"`
    /// by `meta_only`.
    pub runtime: String,
    /// Accounts brief: which discovered account this row's agent runs
    /// under -- `""` (never persisted as `"default"`) means the agent's
    /// own default config folder, today's behaviour exactly. Seeded at
    /// `workspace.create` and, since ADR 0046 decision 6, the ONE field
    /// `workspace.reauth` rewrites on a live row -- so it is
    /// interior-mutable for the same reason `agent_handle` below is:
    /// `Workspaces::set_account` mutates THIS cell on the SHARED
    /// `Arc<Workspace>` in place, never through a replacement `insert`
    /// (which would blank the declared handle and discard the row's
    /// resource caches). Read through `account()` outside this module.
    /// An older toml predates this key and loads as `""`, same default.
    pub(crate) account: Mutex<String>,
    /// The sot-comm handle the session inside this workspace actually
    /// DECLARED via `agent.join` (ADR 0046 decision 1) — distinct from
    /// `agent_name` above, which is only the handle the workspace was
    /// CREATED to expect. `""` (never joined) for a fresh row or an older
    /// toml that lacks the key. Interior-mutable (manager review, S6):
    /// `Workspaces::set_agent_handle` mutates THIS SAME cell on the
    /// SHARED `Arc<Workspace>` in place — never a replacement `insert` —
    /// so a join can never discard the resource caches living alongside
    /// it on the same struct instance. Read through the `agent_handle()`
    /// method outside this module — the field itself stays `pub(crate)`
    /// only for same-crate construction of a freshly built, not-yet-shared
    /// `Workspace` (`from_toml`, test fixtures), never for reading the
    /// live value, which needs the getter's poison-recovery.
    pub(crate) agent_handle: Mutex<String>,
    /// Never persisted; written only through [`Workspace::apply_phase_observation`].
    phase_cell: Mutex<PhaseCell>,
    /// `Some(token)` for exactly as long as a watchdog task owns
    /// restarting this row's own daemon-spawned child. An OWNERSHIP
    /// TOKEN, not an identity: every read is `is_some()` plus a
    /// compare-and-clear, so what the value must guarantee is only that
    /// no two watchdogs can ever hold the same one. A per-daemon counter
    /// gives that outright, where a supervisor identity gave it merely
    /// by luck — two successive legs can share a pid AND a creation
    /// tick, and a superseded watchdog's belated cleanup would then
    /// erase its replacement's ownership. The one fact
    /// `ensure_started_locked`/`resume_locked` consult before spawning a
    /// resume, so activation never races the watchdog's own
    /// backoff/restart budget. `None` for a row with no watchdog: never
    /// started, terminal, or a live authority merely ADOPTED at boot.
    watchdog_owner: Mutex<Option<u64>>,
    /// The most recent FAILED start-on-attach activation's detail, kept
    /// until the next attempt — never implies `phase == Terminal`.
    activation_error: Mutex<Option<String>>,
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

/// Shared registry. Wrapped in `RwLock` because handlers read on every
/// op but mutation is rare (workspace.create / workspace.destroy /
/// startup scan). Cloning a `Workspaces` clones the Arc.
#[derive(Clone, Default)]
pub struct Workspaces {
    inner: Arc<RwLock<Inner>>,
    /// The run gate, beside the registry rather than inside it: a start
    /// asks it before taking any row's guard, and the shutdown waits on it
    /// without holding the registry.
    gate: Arc<(Mutex<RunGate>, Condvar)>,
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
    monitor_hub: Option<crate::sidecars::monitor::MonitorHub>,
    /// The shared preview.changed bus for per-workspace watcher spawns
    /// (2026-07-10 multiwatch). Installed once at startup via
    /// `set_watch_bus`, before workspace registration; `None` in tests. No
    /// longer paired with a `Session` handle (2026-09 rework): `preview.changed`
    /// is no longer bumped onto the session ring — see `watcher.rs`'s header
    /// comment.
    watch_bus: Option<broadcast::Sender<PreviewChanged>>,
    /// One lifecycle-observer task per capsule row; the paired closure cancels its
    /// held connection since `abort()` alone can't stop a round blocked in `spawn_blocking`.
    observers: HashMap<String, (tokio::task::JoinHandle<()>, Arc<dyn Fn() + Send + Sync>)>,
    /// ADR 0043 decision 33: one guard per capsule row. Every lifecycle
    /// mutation of a row — spawn, resume, a watchdog's restart, destroy —
    /// holds this mutex for its WHOLE duration and rechecks membership
    /// and phase once it actually has the lock, so at most one actor can
    /// ever touch a row's supervisor at a time. Replaces the old
    /// `starting: HashSet<String>` claim: that flag guarded only a spawn
    /// attempt and was released the moment the lane first answered or the
    /// leg exited — BEFORE the watchdog's own
    /// restart backoff — so a stale attach landing mid-backoff was free
    /// to spawn a second authority. Created on demand
    /// (`Workspaces::capsule_guard`, under the same write lock this map
    /// itself uses, so two concurrent first-callers for one never-before-
    /// seen id can never mint two different mutexes); dropped with the
    /// row (`remove_by_id`). `Arc` so a caller can hold its own clone
    /// across an `.await` without holding the registry's own `RwLock` —
    /// blocking callers (`resume_if_absent`, `ensure_started`) take
    /// `blocking_lock`, the watchdog's restart arm takes `lock().await`,
    /// `resume_all` takes `try_lock` and skips a row already busy. Named
    /// for its original capsule use, but `capsule_guard` mints/returns
    /// one for ANY registered row regardless of runtime — `agent.join`
    /// and `workspace.destroy`'s tmux arm both take it too now (manager
    /// review round 3), so it guards every row's lifecycle, not only a
    /// capsule's.
    capsule_guards: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

/// A row's session name for the given label, `sot-be-<slug>` — the
/// token `pty.open` / `lane.connect` `target` address the row by (see
/// `Workspace::session_name`). The authoritative naming rule; the
/// frontend mirrors it when it builds a target from a slug.
pub fn session_name(label: &str) -> String {
    format!("sot-be-{}", slug(label))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_uses_slug() {
        assert_eq!(session_name("MyPackage.jl"), "sot-be-mypackage_jl");
    }
}

/// Every op that answers a missing row through `row_or_reply` sends this one reply, byte for byte.
#[cfg(test)]
mod unknown_workspace_tests {
    use serde_json::json;
    use sot_protocol::Kind;

    use crate::server::reply::HandlerOutput;
    use crate::session::Session;

    use super::Workspaces;

    fn assert_unknown_workspace(out: HandlerOutput, op: &str, hint: &str) {
        assert_eq!(out.len(), 1, "{op}: one frame");
        let (frame, blob) = &out[0];
        assert!(blob.is_none(), "{op}: no blob");
        assert_eq!(frame.id, 7, "{op}: request id");
        assert!(matches!(frame.kind, Kind::Res), "{op}: kind res");
        assert_eq!(frame.op, op);
        assert_eq!(frame.rev, None, "{op}: no revision");
        assert_eq!(
            frame.payload,
            json!({ "error": format!("unknown workspace: {hint}"), "code": "unknown_workspace" }),
            "{op}: payload"
        );
        assert_eq!(
            frame.payload.to_string(),
            format!(r#"{{"code":"unknown_workspace","error":"unknown workspace: {}"}}"#, hint.replace('"', "\\\"")),
            "{op}: payload bytes"
        );
    }

    const NOSUCH: &str = "Some(\"ws-nosuch\")";

    macro_rules! unknown_workspace_case {
        ($name:ident, $op:expr, $handler:path, $payload:expr) => {
            #[tokio::test]
            async fn $name() {
                let out = $handler(7, $payload, &Session::new(), &Workspaces::new())
                    .await
                    .expect("handler answers");
                assert_unknown_workspace(out, $op, NOSUCH);
            }
        };
    }

    unknown_workspace_case!(
        tree_root,
        "tree.root",
        crate::files::tree_ops::handle_tree_root,
        json!({ "mode": "files", "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        tree_children,
        "tree.children",
        crate::files::tree_ops::handle_tree_children,
        json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        nav_toggle_hidden,
        "nav.toggle_hidden",
        crate::files::tree_ops::handle_nav_toggle_hidden,
        json!({ "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        preview_get,
        "preview.get",
        crate::files::preview::handle_preview_get,
        json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        preview_set_scale,
        "preview.set_scale",
        crate::files::preview::scale::handle_preview_set_scale,
        json!({ "node_id": "x", "physical_scale": null, "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        image_crop,
        "image.crop",
        crate::files::preview::crop::handle_image_crop,
        json!({ "node_id": "x", "x": 0, "y": 0, "w": 1, "h": 1, "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        concept_read,
        "concept.read",
        crate::files::concept_ops::handle_concept_read,
        json!({ "target": "t", "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        concept_write,
        "concept.write",
        crate::files::concept_ops::handle_concept_write,
        json!({ "target": "t", "content": "c", "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        concept_list,
        "concept.list",
        crate::files::concept_ops::handle_concept_list,
        json!({ "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        file_read,
        "file.read",
        crate::files::io_ops::handle_file_read,
        json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        file_write,
        "file.write",
        crate::files::io_ops::handle_file_write,
        json!({ "node_id": "x", "content": "c", "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        file_delete,
        "file.delete",
        crate::files::io_ops::handle_file_delete,
        json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        dir_create,
        "dir.create",
        crate::files::io_ops::handle_dir_create,
        json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        repl_eval,
        "repl.eval",
        crate::sidecars::repl::ops::handle_repl_eval,
        json!({ "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        repl_run_file,
        "repl.run_file",
        crate::sidecars::repl::ops::handle_repl_run_file,
        json!({ "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        repl_interrupt,
        "repl.interrupt",
        crate::sidecars::repl::ops::handle_repl_interrupt,
        json!({ "workspace_id": "ws-nosuch" })
    );
    unknown_workspace_case!(
        kernel_request,
        "kernel.request",
        crate::sidecars::ops::handle_kernel_request,
        json!({ "kernel_op": "k", "workspace_id": "ws-nosuch" })
    );

    #[tokio::test]
    async fn tree_root_without_a_hint_prints_none() {
        let out = crate::files::tree_ops::handle_tree_root(
            7,
            json!({ "mode": "files" }),
            &Session::new(),
            &Workspaces::new(),
        )
        .await
        .expect("handler answers");
        assert_unknown_workspace(out, "tree.root", "None");
    }

    #[tokio::test]
    async fn file_read_without_a_hint_prints_none() {
        let out = crate::files::io_ops::handle_file_read(
            7,
            json!({ "node_id": "x" }),
            &Session::new(),
            &Workspaces::new(),
        )
        .await
        .expect("handler answers");
        assert_unknown_workspace(out, "file.read", "None");
    }
}
