// server.rs — listener orchestration + transport-agnostic per-connection task.
//
// Spawns the local-socket listener (interprocess) per the Opts the user
// passed. Each accepted stream gets split into AsyncRead/AsyncWrite halves
// and handed to a generic `handle_connection`. The daemon TCP listener (and
// its app-token gate) was removed in 0.4.0 — see ADR 0010's update block;
// the socket's boundary is OS ownership of its private parent path.
//
// Conventional socket strings:
//   Linux/Mac: filesystem path,  e.g. `/tmp/sot-spike.sock`
//   Windows:   named pipe,       e.g. `\\.\pipe\sot-spike`
// interprocess accepts both verbatim via `GenericFilePath::to_fs_name`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use sot_protocol::{
    codec, op, FeCommandEvt, Frame, HostLatest, Kind, MonitorTickEvt,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::clients::{ClientGuard, Clients};
use crate::files_mode::FilesMode;
use crate::handlers;
use crate::mathjax::MathJax;
use crate::paths;
use crate::pages::start_page_servers;
use crate::pluto::Pluto;
use crate::repl::ReplFrameMsg;
use crate::session::Session;
use crate::watcher::PreviewChanged;
use crate::workspaces::{AgentMessage, AgentReceipt};
use crate::workspaces::WorkspaceChanged;
use crate::workspaces::{self, Workspaces};
use crate::Opts;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::task::JoinSet;

pub(super) mod conn;
mod dispatch;
mod events;
pub(super) mod hello;
pub(crate) mod pipe;
pub(crate) mod listen;
pub(super) mod reply;

pub(crate) use crate::rows::ops::pty::record_test_activation_marker;
#[cfg(unix)]
pub(crate) use listen::refuse_live_socket;
pub(crate) use reply::{write_frame_to, write_frame_within};
use listen::{run_local, take_daemon_lock};
use crate::comm::registry::poll::spawn_registry_poll;
use crate::rows::anchor::seed_default_row;

pub async fn run(opts: Opts) -> Result<()> {
    // Lock first: one daemon per state root, held until this process ends
    // (a kill included), so a successor never reads the registry while its
    // predecessor is still shutting down. With no state root there is no
    // record to fence, the same posture as the resume skip below.
    let _daemon_lock = take_daemon_lock(&opts).await?;

    // ADR 0046 decision 1: resolve this daemon's declared host at boot,
    // fatal if it can't be named, so the failure is a boot error rather
    // than a per-hello one. Pin the (bare, S4) own-listener endpoint from
    // `opts.socket` before it's consumed by value below; it is read by
    // `pty::awareness_env` for every pane/capsule this daemon ever spawns
    // (SOT_SOCKET only — the declared host is never pinned into a spawned
    // child's env; see `awareness_env`'s own doc).
    let _ = crate::workspaces::declared_host();
    if let Some(path) = opts.socket.as_deref() {
        crate::awareness::set_own_endpoint(path);
    }

    let session = Session::new();
    let (sid, _) = session.snapshot().await;
    tracing::info!(session_id = %sid, "session ready");

    let files_mode = Arc::new(FilesMode::new(opts.project_root.clone())?);
    tracing::info!(project_root = ?files_mode.root_path(), "files-mode ready");

    // Workspace registry (ADR 0014). Read every persisted workspace off
    // disk, then synthesize and register the *default* workspace (the
    // one this daemon was launched with, rooted at `--project-root`).
    // The default-id resolves to whichever workspace_id matches; for a
    // fresh first-launch we generate one and persist it so subsequent
    // runs see the same id.
    let workspaces = Workspaces::new();
    // A plain "no toml found" is already fail-soft inside `scan_disk`
    // (`Ok(0)`, never `Err`) — the only realistic source of an `Err` here
    // is the Windows legacy-config-dir migration's refuse-and-record path
    // (`workspaces::migrate_legacy_windows_config_dir`'s doc): a rename
    // that fails partway leaves the registry split across the old and new
    // roots, and continuing with whatever landed at the new root (empty or
    // partial) would silently seed a fresh registry beside a stranded one.
    // So this is a boot error, not a warning.
    let n = workspaces::scan_disk(&workspaces, opts.adopt_legacy_registry)
        .context("scanning the workspace registry")?;
    tracing::info!(count = n, "workspaces scanned from disk");
    // The daemon holds the link to the hub and files for its own comm folder
    // (0031 Part 3); a box with no such link returns at once.
    tokio::spawn(crate::hub_link::run(workspaces.clone()));
    // Held until `run` returns: the boot-time row object keeps its watcher and children alive, as before the cut.
    let _default_ws = seed_default_row(&opts, &files_mode, &workspaces);


    // Lazily-spawned MathJax sidecar. Cheap to construct (no child process
    // until the first math.render call); cloning the handle is cheap.
    let mathjax = MathJax::new(MathJax::default_script_path());

    // Lazily-spawned Pluto sidecar. One shared Pluto server per backend,
    // preferring 127.0.0.1:1234 (ephemeral fallback when taken — the daemon
    // learns the actual port from the READY line); spawned on the first
    // `pluto.open`.
    let pluto = Pluto::new(Pluto::default_project_dir(), Pluto::default_start_script());

    start_page_servers().await;

    // Streamed REPL frame bus (Option B): every eval's frames are fanned out
    // here off the per-workspace REPL supervisor; each connection subscribes
    // and writes a `repl.frame` evt frame (mirror of the agent-relay bus,
    // minus a client→daemon publish leg — the publisher is the supervisor).
    // Created before the per-workspace REPLs so it can be installed into the
    // registry (`set_repl_frame_tx`).
    let (repl_frame_tx, _repl_frame_rx) = broadcast::channel::<ReplFrameMsg>(256);
    workspaces.set_repl_frame_tx(repl_frame_tx.clone());

    // Shared preview.changed bus (2026-07-10 multiwatch): ONE broadcast
    // channel every connection subscribes to, fed by ONE file watcher PER
    // WORKSPACE (spawned at registration — Workspaces::set_watch_bus also
    // catches up any workspace registered before this line). Previously a
    // single watcher covered only the default workspace root, so no other
    // workspace's nav ever live-refreshed (the documented KNOWN GAP).
    // Events carry the workspace slug; each connection filters on it (and on
    // path-under-root) at write time — `preview_changed_visible` below. Not
    // paired with `session` (the ring handle): `preview.changed` no longer
    // touches the session ring at all — see `watcher.rs`'s header comment.
    let (preview_changed_tx, _preview_changed_rx) =
        broadcast::channel::<PreviewChanged>(256);
    workspaces.set_watch_bus(preview_changed_tx.clone());

    // Workspace lifecycle bus: parallel to the file watcher's broadcast, but
    // typed `WorkspaceChanged`. Handlers publish on a successful create/
    // destroy; each connection subscribes and writes a `workspace.changed`
    // evt frame so the Sessions strip refreshes live (mirror preview.changed).
    let (ws_events_tx, _ws_events_rx) = broadcast::channel::<WorkspaceChanged>(64);

    // The start (ADR 0042 slice L1a's resume, now behind `held.json`):
    // resume every registered capsule row, or end them all without a
    // resume (`startup::begin`). Here because a Cleanup's ends publish on
    // the workspace bus, and before any listener binds, so the record is
    // in the leases before the first grant.
    let leases = crate::startup::begin(sot_log::host::state_dir::sot_state_dir(), &workspaces, &ws_events_tx);
    tokio::spawn(crate::lease::ticker(leases.clone()));

    // Topology write path (plan §B "Editing the master list"): one store
    // per daemon holding the last successfully parsed `hosts.toml`, and a
    // broadcast bus parallel to the workspace one, typed `TopologyChanged`.
    // `topology.set` publishes here after a successful write; `version.query`
    // (and every `topology.*` op) re-reads the file on-demand and publishes
    // the same way when it notices a hand edit nobody else already announced
    // — never a file watcher (the file can live on a network filesystem).
    let topology_store = std::sync::Arc::new(crate::topology_store::TopologyStore::new(
        sot_protocol::topology::locate().unwrap_or_else(|| PathBuf::from("hosts.toml")),
    ));
    let (topo_changed_tx, _topo_changed_rx) = broadcast::channel::<crate::topology_store::TopologyChanged>(16);

    // ADE state-nav live refresh: poll the sot-comm registry and publish a
    // `workspace.changed` whenever an agent's work-state actually changes, so the
    // Sessions strip re-lists LIVE (the FE re-issues workspace.list on the evt).
    // POLL, not notify — the registry is on NFS where inotify is unreliable; the
    // 1.5s tick also coalesces a working agent's periodic status_at re-stamps. The
    // diff excludes `last_seen` so frequent send/poll heartbeats never spam re-lists.
    spawn_registry_poll(&ws_events_tx);


    // Comm wake (0031 B3): types the unread-mail line into rows at a free
    // prompt. Needs the comm home and this machine's capsule state root.
    if let (Some(comm_home), Some(state_root)) =
        (crate::paths::sot_comm_home(), sot_log::host::state_dir::sot_state_dir())
    {
        tokio::spawn(crate::comm_wake::run(comm_home, state_root, workspaces.clone(), crate::comm_wake::TICK));
    }

    // Agent-relay bus: parallel to the workspace bus, typed `AgentMessage`.
    // `agent.send` publishes here; each connection subscribes and writes an
    // `agent.message` evt frame so a message reaches the other machine's
    // in-terminal agent instantly over the SSH-forwarded socket (mirror of
    // the workspace.changed wiring, plus a client→daemon publish leg).
    let (agent_events_tx, _agent_events_rx) = broadcast::channel::<AgentMessage>(256);

    // Filer-receipt bus (ADR 0048): the return leg of the agent relay.
    // `agent.filed` publishes here; each connection writes an
    // `agent.receipt` evt frame, so the SENDER reads its own verdict on the
    // connection it is already holding open. A separate typed bus rather
    // than a second meaning for `AgentMessage` — one bus per event type is
    // this file's own idiom (workspace.changed, fe.command, repl frames,
    // topology) and a receipt is not a message.
    let (agent_receipt_tx, _agent_receipt_rx) = broadcast::channel::<AgentReceipt>(256);

    // FE-command bus (ADR 0025): parallel to the agent-relay bus, typed
    // `FeCommandEvt`. `fe.command.send` publishes here; each connection
    // subscribes and writes an `fe.command` evt frame so an imperative UI
    // command (preview/reveal/goto/notify) reaches every connected frontend
    // instantly over the SSH-forwarded socket. Mirror of the agent_events_tx
    // wiring, plus the same client→daemon publish leg. Broadcast to ALL
    // connections; the FE self-filters on `target`.
    let (fe_command_tx, _fe_command_rx) = broadcast::channel::<FeCommandEvt>(256);

    // Server-monitoring data plane (ADR 0020): always-on samplers (one per
    // host) feeding a tiered ring + the `monitor.tick` broadcast bus. Stored on
    // the registry (mirrors `set_repl_frame_tx`) so every connection can
    // subscribe and the `monitor.*` ops can reach it. Sampling runs for the
    // life of the backend so the drawer shows real history the moment it opens;
    // per-connection tick delivery is gated by `monitor.subscribe`.
    let monitor_hub = crate::monitor::MonitorHub::start(crate::monitor::load_hosts());
    workspaces.set_monitor_hub(monitor_hub);

    // Connected-frontend registry (ADR 0010/0013 multi-frontend). Shared
    // across both listeners so the live count spans transports; each
    // connection registers on hello and deregisters on drop.
    let clients = Clients::new();

    // Auto-updater (ADR 0030 §4, Phase C): daily check that, on a newer
    // release, pushes an `fe.command` `notify` over the bus above and runs
    // the stage → prepare → arm pipeline. In `auto` mode it may also exit
    // for the apply owner when no clients are attached (hence the roster
    // handle). A `-dev` build (the whole fleet) disables it at the hard
    // guard inside `spawn_periodic`, which logs the disabled state and
    // returns without spawning anything.
    crate::update::spawn_periodic(fe_command_tx.clone(), clients.clone(), leases.clone());

    let label = Arc::new(opts.label);
    let mut tasks: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();

    if let Some(path) = opts.socket {
        let s = session.clone();
        let mj = mathjax.clone();
        let pl = pluto.clone();
        let fm = files_mode.clone();
        let wa = preview_changed_tx.clone();
        let lb = label.clone();
        let ws = workspaces.clone();
        let wse = ws_events_tx.clone();
        let age = agent_events_tx.clone();
        let agr = agent_receipt_tx.clone();
        let fce = fe_command_tx.clone();
        let rfe = repl_frame_tx.clone();
        let cl = clients.clone();
        let tps = topology_store.clone();
        let tpe = topo_changed_tx.clone();
        let le = leases.clone();
        tasks.push(tokio::spawn(async move {
            run_local(
                path, s, mj, pl, fm, wa, lb, ws, wse, age, agr, fce, rfe, cl, tps,
                tpe, le,
            )
            .await
        }));
    }

    if tasks.is_empty() {
        anyhow::bail!("no listener configured");
    }

    // Wait for whichever listener errors first; on a clean run they loop
    // forever, so this only returns on a real failure.
    let (res, _idx, _rest) = futures_util::future::select_all(tasks).await;
    res.context("listener task panicked")??;
    Ok(())
}
