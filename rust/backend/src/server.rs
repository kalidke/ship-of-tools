// server.rs — listener orchestration + transport-agnostic per-connection task.
//
// Spawns one or both transport listeners (local socket via interprocess,
// TCP via tokio::net) per the Opts the user passed. Each accepted stream
// gets split into AsyncRead/AsyncWrite halves and handed to a generic
// `handle_connection` — the wire format and dispatch are identical on
// either transport.
//
// Auth: TCP enforces an app-level token when configured (`--token`, SOT_TOKEN,
// or the canonical token file), and `main.rs` refuses a tokenless TCP listener
// unless `--insecure-no-auth` is explicit. Local sockets do not enforce app
// tokens; their boundary is OS ownership of the private socket path.
//
// Conventional socket strings:
//   Linux/Mac: filesystem path,  e.g. `/tmp/sot-spike.sock`
//   Windows:   named pipe,       e.g. `\\.\pipe\sot-spike`
// interprocess accepts both verbatim via `GenericFilePath::to_fs_name`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine;
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    GenericFilePath, ListenerOptions,
};
use sot_protocol::{
    codec, op, FeCommandEvt, Frame, HostLatest, Kind, MonitorHistoryReq, MonitorHistoryRes,
    MonitorSubscribeRes, MonitorTickEvt, PtyOpenReq, PtyOpenRes, PtyResizeReq, PtyWriteReq,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

use crate::clients::Clients;
use crate::concept::ConceptStore;
use crate::files_mode::FilesMode;
use crate::handlers;
use crate::kernel::Kernel;
use crate::mathjax::MathJax;
use crate::paths;
use crate::pluto::Pluto;
use crate::pty::Pty;
use crate::repl::{Repl, ReplFrameMsg};
use crate::session::Session;
use crate::watcher::{PreviewChanged, Watcher};
use crate::workspaces::AgentMessage;
use crate::workspaces::WorkspaceChanged;
use crate::workspaces::{self, Workspace, Workspaces};
use crate::Opts;
use tokio::sync::broadcast;

// Half-open connection reaper tunables (ADR 0027). A peer that dies without a
// FIN — a frontend killed -9, a collapsed SSH local-forward, a yanked network
// — leaves the daemon-side socket ESTAB forever; without these two mechanisms
// its task leaks (fd + ClientGuard) and, if blocked mid-write, never reads the
// socket again (the broadcast-stall we hit). See `run_tcp` (keepalive) and
// `write_frame_to` (write-timeout).
//
// Keepalive reaps the IDLE half-open case (nothing to write, task parked in the
// read arm): the OS probes after ~20s idle, 3 × 10s apart, then RSTs (~50s) →
// the read returns Err → the connection drops. The write-timeout reaps the
// ACTIVE-but-not-draining case (the witnessed Recv-Q stall) faster and on any
// transport. Both are needed; the write-timeout is the critical anti-stall one.
const KEEPALIVE_IDLE: std::time::Duration = std::time::Duration::from_secs(20);
const KEEPALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
const KEEPALIVE_RETRIES: u32 = 3;

/// Base deadline for a single frame write before we treat the peer as dead and
/// drop the connection. Small control frames — even over the SSH tunnel — drain
/// in well under a second, so 10s catches a wedged/non-draining peer fast without
/// parking a connection task. A rare false drop is cheap: the FE reconnects
/// automatically (exponential backoff).
///
/// This is a FLOOR, not the whole story: a legitimate bulk blob (a 71 MB
/// scientific render riding the codec's blob tail) can't drain in 10s over a
/// tunnel, and a flat 10s per-frame cap false-dropped it mid-write — the reaper
/// firing on a transfer that WAS draining, so the preview silently never arrived
/// (2026-06-30, example-paper render_sr.png). The deadline is therefore SCALED by
/// the blob size (see `write_deadline`): base + size/`MIN_BLOB_DRAIN_RATE`.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Floor drain rate we credit a healthy peer for the bulk blob tail. A real
/// transfer sustains far more than this over a tunnel; setting the floor low
/// keeps the size-scaled deadline generous enough never to false-drop a draining
/// blob, while a genuinely wedged peer (zero progress) is still bounded here and
/// independently reaped by SO_KEEPALIVE (~50s of dead TCP). 1 MiB/s → a 71 MB
/// render gets ~71s on top of the base floor.
const MIN_BLOB_DRAIN_RATE: u64 = 1024 * 1024; // bytes/sec

/// Per-frame write deadline: the [`WRITE_TIMEOUT`] floor plus one second of grace
/// per `MIN_BLOB_DRAIN_RATE` bytes of blob. Envelope-only / small-blob frames get
/// the tight floor (reaper stays sharp); a large preview blob gets proportional
/// time to drain so a legit transfer isn't reaped mid-write.
fn write_deadline(blob: Option<&[u8]>) -> std::time::Duration {
    let extra = blob.map_or(0, |b| b.len() as u64 / MIN_BLOB_DRAIN_RATE);
    WRITE_TIMEOUT + std::time::Duration::from_secs(extra)
}

/// Write one frame to a connection with a bounded timeout (ADR 0027, reaper
/// half 2). On timeout we return an error so `handle_connection` unwinds and
/// drops the connection: a peer that hasn't drained a single frame in
/// `WRITE_TIMEOUT` is dead or wedged, and a parked write would otherwise hold
/// the task forever — never reading the socket, never releasing its
/// `ClientGuard`. Cancel-safety is irrelevant on the timeout path: we tear the
/// whole socket down, so a partially written frame is moot. Every
/// per-connection evt/response write goes through this.
async fn write_frame_to<W>(tx: &mut W, frame: &Frame, blob: Option<&[u8]>) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    write_frame_within(tx, frame, blob, write_deadline(blob)).await
}

/// Timeout-parameterized core of [`write_frame_to`], split out so the reaper's
/// drop-on-stuck-peer behavior is unit-testable in milliseconds rather than the
/// production `WRITE_TIMEOUT`.
async fn write_frame_within<W>(
    tx: &mut W,
    frame: &Frame,
    blob: Option<&[u8]>,
    timeout: std::time::Duration,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match tokio::time::timeout(timeout, codec::write_frame(tx, frame, blob)).await {
        Ok(inner) => {
            inner?;
            Ok(())
        }
        Err(_elapsed) => anyhow::bail!(
            "frame write exceeded {timeout:?}; dropping connection (peer not draining)"
        ),
    }
}

/// Derive an agent's work-state from a snapshot of its live tmux pane (the
/// claude TUI footer). Authoritative for working/idle; needs no hook or model
/// cooperation — the `Stop` hook only ever reports idle.
///
/// `working` keys off claude's live generation status line, which starts with a
/// "sparkle" spinner glyph and carries a parenthesised elapsed timer, e.g.
/// `✽ Wiring weakdep extensions… (3m 16s · ↓ 14.8k tokens · …)`. BOTH parts
/// matter:
///   - the spinner prefix means ordinary output can't fake it — prose (even this
///     session quoting the footer in a report) never *starts a line* with a
///     sparkle glyph, so the bare-phrase contamination that read an idle pane as
///     working is gone; and
///   - the parenthesised timer excludes the post-turn summary, which is also
///     spinner-led but reads `Churned for 17m 59s` (no parens) once idle.
/// The status line can sit well above the input box (a todo list / tool output
/// between it and the prompt), so we scan the whole capture for it — a fixed
/// 12-line footer window read a working agent *with a todo list* as idle because
/// its status line was 15 lines up.
///
/// Returns `""` (no claude → registry fallback), `"working"`, or `"idle"`.
/// Pure + total: unit-testable, can't panic in the capture loop.
pub(crate) fn pane_activity(contents: &str) -> &'static str {
    let lines: Vec<&str> = contents.lines().collect();
    // claude present? The persistent footer hint (permission mode / shortcuts)
    // sits in the last lines. A bare shell has none → registry fallback.
    let footer_start = lines.len().saturating_sub(12);
    let footer = lines[footer_start..].join("\n").to_lowercase();
    const PRESENT: &[&str] = &[
        "bypass permissions",
        "bypassing permissions",
        "for shortcuts",
        "for agents",
    ];
    if !PRESENT.iter().any(|m| footer.contains(m)) {
        return "";
    }
    // Generating? Find the live status line: spinner-led AND a running timer.
    if lines
        .iter()
        .any(|l| line_starts_with_spinner(l) && has_running_timer(l))
    {
        "working"
    } else {
        "idle"
    }
}

/// True if the line's first non-whitespace char is one of claude's "sparkle"
/// spinner glyphs (the generating indicator cycles dingbats in U+2722–U+2747:
/// ✢ ✦ ✶ ✷ ✸ ✹ ✺ ✻ ✼ ✽ …). The other line-leading glyphs the TUI uses — `●`
/// (U+25CF) messages, `⎿` (U+23BF) tool output, `◼ ◻ ✔` todo items, `❯`
/// (U+276F) the prompt — all fall outside this range, so they never false-trip.
fn line_starts_with_spinner(line: &str) -> bool {
    matches!(
        line.trim_start().chars().next(),
        Some(c) if ('\u{2722}'..='\u{2747}').contains(&c)
    )
}

/// True if `s` contains a parenthesised elapsed timer like `(45s`, `(2m 55s` —
/// the counter claude renders on the live status line. Dependency-free scan (no
/// regex) so the capture loop stays panic-proof.
fn has_running_timer(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'(' {
            let mut j = i + 1;
            let mut saw_digit = false;
            while j < b.len() && b[j].is_ascii_digit() {
                saw_digit = true;
                j += 1;
            }
            // digit(s) immediately followed by a time unit → a live timer.
            if saw_digit && j < b.len() && (b[j] == b'm' || b[j] == b's') {
                return true;
            }
        }
        i += 1;
    }
    false
}

pub async fn run(opts: Opts) -> Result<()> {
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
    match workspaces::scan_disk(&workspaces) {
        Ok(n) => tracing::info!(count = n, "workspaces scanned from disk"),
        Err(e) => {
            tracing::warn!(error = %e, "workspace scan failed; continuing with empty registry")
        }
    }
    let default_label = opts
        .label
        .clone()
        .or_else(|| {
            files_mode
                .root_path()
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "home".to_string());
    let mut default_ws_seed = Workspace::from_label(
        &default_label,
        files_mode.root_path().to_path_buf(),
        false,
        "none".to_string(),
        String::new(),
        String::new(),
    );
    // Display the Ship of Tools home/default workspace as ".SoT" — the leading
    // dot is cosmetic (the FE strip renders the label) and marks it as "home".
    // Only the LABEL changes; the slug (hence the tmux session `sot-be-sot` and
    // this daemon's own pane + comm handle) is untouched. Gated to the sot project
    // so a daemon launched on another repo keeps its own name.
    if default_ws_seed.slug == "sot" {
        default_ws_seed.label = ".SoT".to_string();
    }
    workspaces.insert(default_ws_seed);
    let default_ws = workspaces
        .resolve(Some(&paths::slug(&default_label)))
        .expect("default workspace just inserted");
    workspaces.set_default(&default_ws.workspace_id);
    if let Err(e) = workspaces::save(&default_ws) {
        tracing::warn!(error = %e, "could not persist default workspace toml");
    } else {
        tracing::info!(
            workspace_id = %default_ws.workspace_id,
            slug = %default_ws.slug,
            "default workspace ready"
        );
    }
    // Ensure the default workspace's tmux session exists so it shows up
    // in Sessions mode alongside any user-created workspaces. `tmux
    // new-session -A` is idempotent — already-alive sessions are a
    // no-op. Failure is logged but non-fatal (a head-less host with no
    // tmux server still serves the protocol fine).
    {
        let tmux_name = default_ws.tmux_session.clone();
        let cwd = default_ws.project_root.clone();
        let _ = tokio::task::spawn_blocking(move || {
            match crate::tmux::TmuxClient::new().create_session(&tmux_name, None, Some(&cwd)) {
                Ok(()) => tracing::info!(tmux = %tmux_name, "default workspace tmux session ready"),
                Err(e) => tracing::warn!(error = %e, tmux = %tmux_name, "default workspace tmux session create failed"),
            }
        })
        .await;
    }

    // When the backend is launched with `--label`, stamp our identity into
    // `~/.config/sot/sessions/<slug>.toml` so Sessions mode (frontend)
    // can discover us — including direct shell launches that bypassed
    // tmux.create_session. Frontend-managed sections are preserved.
    if let Some(label) = opts.label.as_deref() {
        match crate::session_state::write_backend_identity(
            label,
            &sid,
            files_mode.root_path(),
            opts.socket.as_deref(),
        ) {
            Ok(path) => {
                tracing::info!(toml = ?path, "wrote backend identity toml");
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not write backend identity toml; Sessions mode discovery may need help");
            }
        }
    }

    // Concept-annotation store at `<project_root>/.concept/`. Created lazily
    // on first write — read/list against a missing directory return empty.
    let concept = Arc::new(ConceptStore::new(files_mode.root_path()));

    // Lazily-spawned MathJax sidecar. Cheap to construct (no child process
    // until the first math.render call); cloning the handle is cheap.
    let mathjax = MathJax::new(MathJax::default_script_path());

    // Lazily-spawned Pluto sidecar. One shared Pluto server per backend
    // listening on 127.0.0.1:1234; spawned on the first `pluto.open`.
    let pluto = Pluto::new(Pluto::default_project_dir(), Pluto::default_start_script());

    // Loopback video file server for browser playback (ADR 0018). Bound at
    // startup so `video.open` URLs are immediately reachable once the launcher
    // forwards the port. Serves only video files, 127.0.0.1 only.
    if let Err(e) = crate::http_serve::spawn(crate::http_serve::video_port()).await {
        tracing::warn!(error = %e, "video http server failed to start; `o` on a video won't work");
    }

    // Loopback static-site server (ADR 0024). Serves ANY on-disk static site —
    // its root is set per-open by the `docs.open` handler to the cursored file's
    // directory — so `W` opens whatever site/page is selected (HTML/CSS/JS/assets/
    // sub-paths) in the OS browser with full fidelity once the launcher forwards
    // the port. 127.0.0.1 only; workspace-agnostic.
    if let Err(e) = crate::site_serve::spawn(crate::site_serve::site_port()).await {
        tracing::warn!(error = %e, "static-site server failed to start; `W` won't work");
    }
    // ADR 0029 Option B: the dedicated-port pool for root-relative sites
    // (an example project's __site etc.). Per-port bind failures shrink the pool, never
    // fatal — docs.open reports "slots busy" when none are assignable.
    crate::site_serve::spawn_pool().await;

    // Lazily-spawned Julia kernel — only fires up when first kernel.request
    // op arrives. The Files-mode walker handles the no-Julia case fine on
    // its own, so this stays a feature flag of sorts.
    let kernel = Kernel::new(
        Kernel::default_kernel_project(),
        files_mode.root_path().to_path_buf(),
    );

    // Streamed REPL frame bus (Option B): every eval's frames are fanned out
    // here off the per-workspace REPL supervisor; each connection subscribes
    // and writes a `repl.frame` evt frame (mirror of the agent-relay bus,
    // minus a client→daemon publish leg — the publisher is the supervisor).
    // Created before the per-workspace REPLs so it can be installed into the
    // registry (`set_repl_frame_tx`) and threaded into the legacy singleton.
    let (repl_frame_tx, _repl_frame_rx) = broadcast::channel::<ReplFrameMsg>(256);
    workspaces.set_repl_frame_tx(repl_frame_tx.clone());

    // Persistent REPL — separate Julia child from the kernel so a runaway
    // eval can't take down introspection. Lazy spawn. The singleton is
    // retained for back-compat on the call chain; all ops now route through
    // per-workspace REPLs (which carry their own workspace_id), so this one
    // publishes with `None` as its workspace_id.
    let repl = Repl::new(Repl::default_repl_project(), repl_frame_tx.clone(), None);

    // Notify-based file watcher: pushes preview.changed evts when files
    // under the project root mutate on disk. Bumps the session ring so
    // reconnecting clients catch changes that landed while away. Failure
    // to start is a warning, not fatal — previews still work, they just
    // won't auto-refresh.
    let watcher = match Watcher::spawn(files_mode.root_path(), session.clone(), files_mode.clone())
    {
        Ok(w) => Some(Arc::new(w)),
        Err(e) => {
            tracing::warn!(error = %e, "file watcher unavailable; previews will not auto-refresh on disk changes");
            None
        }
    };

    // Workspace lifecycle bus: parallel to the file watcher's broadcast, but
    // typed `WorkspaceChanged`. Handlers publish on a successful create/
    // destroy; each connection subscribes and writes a `workspace.changed`
    // evt frame so the Sessions strip refreshes live (mirror preview.changed).
    let (ws_events_tx, _ws_events_rx) = broadcast::channel::<WorkspaceChanged>(64);

    // ADE state-nav live refresh: poll the sot-comm registry and publish a
    // `workspace.changed` whenever an agent's work-state actually changes, so the
    // Sessions strip re-lists LIVE (the FE re-issues workspace.list on the evt).
    // POLL, not notify — the registry is on NFS where inotify is unreliable; the
    // 1.5s tick also coalesces a working agent's periodic status_at re-stamps. The
    // diff excludes `last_seen` so frequent send/poll heartbeats never spam re-lists.
    if let Some(reg_path) = crate::handlers::comm_registry_path() {
        let tx = ws_events_tx.clone();
        tokio::spawn(async move {
            // Canonical projection of just the state-relevant fields per agent
            // (state/summary/status_at + tmux, since tmux drives the live-occupant
            // binding). `last_seen` is deliberately excluded.
            fn project(bytes: &[u8]) -> String {
                let root: serde_json::Value = match serde_json::from_slice(bytes) {
                    Ok(v) => v,
                    Err(_) => return String::new(),
                };
                let agents = match root.get("agents").and_then(|a| a.as_object()) {
                    Some(o) => o,
                    None => return String::new(),
                };
                let mut keys: Vec<&String> = agents.keys().collect();
                keys.sort();
                let mut s = String::new();
                for k in keys {
                    let e = &agents[k];
                    s.push_str(k);
                    for field in ["state", "summary", "status_at", "tmux"] {
                        s.push('\u{1}');
                        s.push_str(e.get(field).and_then(|v| v.as_str()).unwrap_or(""));
                    }
                    s.push('\u{2}');
                }
                s
            }
            let mut last: Option<String> = None;
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(1500));
            loop {
                tick.tick().await;
                if let Some(cur) = tokio::fs::read(&reg_path).await.ok().map(|b| project(&b)) {
                    if last.as_ref().map_or(false, |p| *p != cur) {
                        let _ = tx.send(WorkspaceChanged {
                            action: "agent_state".into(),
                            slug: String::new(),
                            workspace_id: String::new(),
                        });
                    }
                    last = Some(cur);
                }
            }
        });
    }

    // ADE state-nav pane-watch: the registry-watch above only catches
    // comm-status / registry writes, which on the work-state axis only ever say
    // "idle" (the `Stop` hook). There is no "working" hook, so an actively-
    // generating agent reads idle. This task derives working/idle LIVE from each
    // workspace's claude pane (its TUI footer) — authoritative, needing no hook,
    // restart, or model cooperation. Every ~2s it captures every workspace's
    // pane (each capture in `spawn_blocking` so a slow/hung `tmux capture-pane`
    // never stalls the runtime, and fully defensive — a capture error yields ""
    // activity, never a panic), derives state via `pane_activity`, and caches it
    // on `Workspaces`. When ANY workspace's activity changed vs the previous
    // tick it sends ONE `agent_state` WorkspaceChanged so the FE re-lists and
    // picks up the new pane-derived state (coalesced — one evt covers all).
    {
        let ws_reg = workspaces.clone();
        let tx = ws_events_tx.clone();
        tokio::spawn(async move {
            let mut last: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tick.tick().await;
                let mut changed = false;
                let mut current: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                for ws in ws_reg.list() {
                    let session = ws.tmux_session.clone();
                    if session.is_empty() || current.contains_key(&session) {
                        continue;
                    }
                    // Capture off-runtime; any failure (no server, dead pane,
                    // non-UTF8) maps to an empty capture → "" activity.
                    let target = session.clone();
                    let contents = tokio::task::spawn_blocking(move || {
                        crate::tmux::TmuxClient::new()
                            .capture_pane(&target, 40)
                            .unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default();
                    let activity = pane_activity(&contents);
                    if last.get(&session).map(String::as_str).unwrap_or("") != activity {
                        changed = true;
                    }
                    ws_reg.set_pane_activity(&session, activity);
                    current.insert(session, activity.to_string());
                }
                // A workspace that vanished (its session gone from the list)
                // also counts as a change so the FE drops its stale state.
                if !changed && current.len() != last.len() {
                    changed = true;
                }
                if changed {
                    let _ = tx.send(WorkspaceChanged {
                        action: "agent_state".into(),
                        slug: String::new(),
                        workspace_id: String::new(),
                    });
                }
                last = current;
            }
        });
    }

    // Agent-relay bus: parallel to the workspace bus, typed `AgentMessage`.
    // `agent.send` publishes here; each connection subscribes and writes an
    // `agent.message` evt frame so a message reaches the other machine's
    // in-terminal agent instantly over the SSH-forwarded socket (mirror of
    // the workspace.changed wiring, plus a client→daemon publish leg).
    let (agent_events_tx, _agent_events_rx) = broadcast::channel::<AgentMessage>(256);

    // FE-command bus (ADR 0025): parallel to the agent-relay bus, typed
    // `FeCommandEvt`. `fe.command.send` publishes here; each connection
    // subscribes and writes an `fe.command` evt frame so an imperative UI
    // command (preview/reveal/goto/notify) reaches every connected frontend
    // instantly over the SSH-forwarded socket. Mirror of the agent_events_tx
    // wiring, plus the same client→daemon publish leg. Broadcast to ALL
    // connections; the FE self-filters on `target`.
    let (fe_command_tx, _fe_command_rx) = broadcast::channel::<FeCommandEvt>(256);

    // Auto-updater (ADR 0030 §4, Phase C): daily check that, on a newer
    // release, pushes an `fe.command` `notify` over the bus above and stages
    // the platform binary. A `-dev` build (the whole fleet) disables it at the
    // hard guard inside `spawn_periodic`, which logs the disabled state and
    // returns without spawning anything.
    crate::update::spawn_periodic(fe_command_tx.clone());

    // Server-monitoring data plane (ADR 0020): always-on samplers (one per
    // host) feeding a tiered ring + the `monitor.tick` broadcast bus. Stored on
    // the registry (mirrors `set_repl_frame_tx`) so every connection can
    // subscribe and the `monitor.*` ops can reach it. Sampling runs for the
    // life of the backend so the drawer shows real history the moment it opens;
    // per-connection tick delivery is gated by `monitor.subscribe`.
    let monitor_hub =
        crate::monitor::MonitorHub::start(crate::monitor::load_hosts(&opts.project_root));
    workspaces.set_monitor_hub(monitor_hub);

    // Connected-frontend registry (ADR 0010/0013 multi-frontend). Shared
    // across both listeners so the live count spans transports; each
    // connection registers on hello and deregisters on drop.
    let clients = Clients::new();

    let token = Arc::new(opts.token);
    let label = Arc::new(opts.label);
    let mut tasks: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();

    if let Some(path) = opts.socket {
        let s = session.clone();
        let tok = Arc::new(None);
        let mj = mathjax.clone();
        let pl = pluto.clone();
        let fm = files_mode.clone();
        let ke = kernel.clone();
        let co = concept.clone();
        let rp = repl.clone();
        let wa = watcher.clone();
        let lb = label.clone();
        let ws = workspaces.clone();
        let wse = ws_events_tx.clone();
        let age = agent_events_tx.clone();
        let fce = fe_command_tx.clone();
        let rfe = repl_frame_tx.clone();
        let cl = clients.clone();
        tasks.push(tokio::spawn(async move {
            run_local(
                path, s, tok, mj, pl, fm, ke, co, rp, wa, lb, ws, wse, age, fce, rfe, cl,
            )
            .await
        }));
    }

    if let Some(addr) = opts.tcp {
        let s = session.clone();
        let tok = token.clone();
        let mj = mathjax.clone();
        let pl = pluto.clone();
        let fm = files_mode.clone();
        let ke = kernel.clone();
        let co = concept.clone();
        let rp = repl.clone();
        let wa = watcher.clone();
        let lb = label.clone();
        let ws = workspaces.clone();
        let wse = ws_events_tx.clone();
        let age = agent_events_tx.clone();
        let fce = fe_command_tx.clone();
        let rfe = repl_frame_tx.clone();
        let cl = clients.clone();
        tasks.push(tokio::spawn(async move {
            run_tcp(
                addr, s, tok, mj, pl, fm, ke, co, rp, wa, lb, ws, wse, age, fce, rfe, cl,
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

async fn run_local(
    socket_path: PathBuf,
    session: Session,
    token: Arc<Option<String>>,
    mathjax: MathJax,
    pluto: Pluto,
    files_mode: Arc<FilesMode>,
    // Singleton handles retained on the call chain for backward compat
    // and to keep the run_local / run_tcp signatures unchanged. All op
    // handlers now route through Workspaces per ADR 0014; these
    // bindings are dead in `handle_connection` itself.
    #[allow(unused_variables, dead_code)] kernel: Kernel,
    #[allow(unused_variables, dead_code)] concept: Arc<ConceptStore>,
    #[allow(unused_variables, dead_code)] repl: Repl,
    watcher: Option<Arc<Watcher>>,
    label: Arc<Option<String>>,
    workspaces: Workspaces,
    ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    agent_events_tx: broadcast::Sender<AgentMessage>,
    fe_command_tx: broadcast::Sender<FeCommandEvt>,
    repl_frame_tx: broadcast::Sender<ReplFrameMsg>,
    clients: Clients,
) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        if !parent.as_os_str().is_empty() {
            paths::secure_socket_dir(parent)
                .with_context(|| format!("secure socket dir {}", parent.display()))?;
        }
    }
    // Unix sockets leave a filesystem entry that blocks rebind; Windows
    // named pipes don't, so only do the cleanup on Unix.
    #[cfg(unix)]
    if std::path::Path::new(&socket_path).exists() {
        tokio::fs::remove_file(&socket_path)
            .await
            .with_context(|| format!("remove stale socket {socket_path:?}"))?;
    }

    let path_str = socket_path
        .to_str()
        .context("socket path must be valid UTF-8")?;
    let name = path_str
        .to_fs_name::<GenericFilePath>()
        .with_context(|| format!("interpret {path_str:?} as local-socket name"))?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_tokio()
        .with_context(|| format!("bind {socket_path:?}"))?;
    tracing::info!(socket = ?socket_path, "listening (local)");

    loop {
        let stream: LocalStream = listener.accept().await.context("accept on sot socket")?;
        let s = session.clone();
        let tok = token.clone();
        let mj = mathjax.clone();
        let pl = pluto.clone();
        let fm = files_mode.clone();
        let ke = kernel.clone();
        let co = concept.clone();
        let rp = repl.clone();
        let wa = watcher.clone();
        let lb = label.clone();
        let ws = workspaces.clone();
        let wse = ws_events_tx.clone();
        let age = agent_events_tx.clone();
        let fce = fe_command_tx.clone();
        let rfe = repl_frame_tx.clone();
        let cl = clients.clone();
        tokio::spawn(async move {
            let (rx, tx) = stream.split();
            if let Err(e) = handle_connection(
                rx, tx, s, tok, mj, pl, fm, ke, co, rp, wa, lb, ws, wse, age, fce, rfe, cl,
                "local", None,
            )
            .await
            {
                tracing::warn!(error = %e, transport = "local", "connection ended with error");
            } else {
                tracing::info!(transport = "local", "connection closed");
            }
        });
    }
}

async fn run_tcp(
    addr: SocketAddr,
    session: Session,
    token: Arc<Option<String>>,
    mathjax: MathJax,
    pluto: Pluto,
    files_mode: Arc<FilesMode>,
    // Singleton handles retained on the call chain for backward compat
    // and to keep the run_local / run_tcp signatures unchanged. All op
    // handlers now route through Workspaces per ADR 0014; these
    // bindings are dead in `handle_connection` itself.
    #[allow(unused_variables, dead_code)] kernel: Kernel,
    #[allow(unused_variables, dead_code)] concept: Arc<ConceptStore>,
    #[allow(unused_variables, dead_code)] repl: Repl,
    watcher: Option<Arc<Watcher>>,
    label: Arc<Option<String>>,
    workspaces: Workspaces,
    ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    agent_events_tx: broadcast::Sender<AgentMessage>,
    fe_command_tx: broadcast::Sender<FeCommandEvt>,
    repl_frame_tx: broadcast::Sender<ReplFrameMsg>,
    clients: Clients,
) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(tcp = %addr, "listening (tcp)");
    // Loud warning if the user bound to a non-loopback address — per ADR
    // 0010 the right cross-machine path is SSH local-forward, not an
    // exposed bind. We don't refuse; we just make the mistake visible.
    if !addr.ip().is_loopback() {
        tracing::warn!(
            tcp = %addr,
            "TCP bound to non-loopback address; expose via SSH local-forward instead per ADR 0010"
        );
    }

    loop {
        let (stream, peer) = listener.accept().await.context("accept on tcp")?;
        // Disable Nagle's algorithm on the accepted socket — interactive
        // keystroke traffic (LLM pane, REPL) is small and frequent, and
        // Nagle batches it with a ~40ms tail per write, masquerading as
        // remote latency even on a loopback / fast LAN link. NODELAY
        // moves bytes immediately; the frontend sets the same flag on its
        // outbound socket so both directions stay snappy.
        if let Err(e) = stream.set_nodelay(true) {
            tracing::warn!(peer = %peer, error = %e, "set_nodelay failed; tcp socket may batch writes");
        }
        // Half-open connection reaper, half 1 (ADR 0027). Enable TCP keepalive
        // with a tight cadence so the OS detects and RSTs a vanished peer — a
        // frontend killed -9, a collapsed SSH local-forward, a network drop —
        // none of which send a FIN, so without this the daemon-side socket sits
        // ESTAB forever, its task parked in the select! read arm (idle) or
        // blocked in a write (active), leaking the fd + its ClientGuard and
        // never reading the socket again. A killed peer that DID send FIN is
        // already handled (read returns EOF); keepalive covers the silent case.
        // ~20s idle then 3 probes 10s apart → reaped ~50s after the peer dies.
        // The companion write-timeout (half 2, in handle_connection) catches the
        // active-but-not-draining case faster and on any transport.
        {
            let ka = socket2::TcpKeepalive::new()
                .with_time(KEEPALIVE_IDLE)
                .with_interval(KEEPALIVE_INTERVAL)
                .with_retries(KEEPALIVE_RETRIES);
            if let Err(e) = socket2::SockRef::from(&stream).set_tcp_keepalive(&ka) {
                tracing::warn!(peer = %peer, error = %e, "set_tcp_keepalive failed; half-open conns may leak until write-timeout");
            }
        }
        let s = session.clone();
        let tok = token.clone();
        let mj = mathjax.clone();
        let pl = pluto.clone();
        let fm = files_mode.clone();
        let ke = kernel.clone();
        let co = concept.clone();
        let rp = repl.clone();
        let wa = watcher.clone();
        let lb = label.clone();
        let ws = workspaces.clone();
        let wse = ws_events_tx.clone();
        let age = agent_events_tx.clone();
        let fce = fe_command_tx.clone();
        let rfe = repl_frame_tx.clone();
        let cl = clients.clone();
        tokio::spawn(async move {
            let (rx, tx) = stream.into_split();
            tracing::info!(peer = %peer, transport = "tcp", "client connected");
            if let Err(e) = handle_connection(
                rx,
                tx,
                s,
                tok,
                mj,
                pl,
                fm,
                ke,
                co,
                rp,
                wa,
                lb,
                ws,
                wse,
                age,
                fce,
                rfe,
                cl,
                "tcp",
                Some(peer.to_string()),
            )
            .await
            {
                tracing::warn!(error = %e, transport = "tcp", peer = %peer, "connection ended with error");
            } else {
                tracing::info!(transport = "tcp", peer = %peer, "connection closed");
            }
        });
    }
}

/// Read one frame while *owning* the buffered reader, handing it back with
/// the result. Lets the per-connection select! loop keep a single in-flight
/// read future across iterations so a cancelled select! *pauses* a mid-blob
/// read rather than dropping it — `codec::read_frame` is NOT cancellation-safe
/// (it reads the `\n` envelope then `read_exact`s the blob tail across two
/// awaits). Mirrors the frontend transport fix (commit 8746b74). See the
/// CANCEL-SAFETY note in `handle_connection`'s loop.
async fn read_owned<R: AsyncRead + Unpin>(
    mut rx: tokio::io::BufReader<R>,
) -> (tokio::io::BufReader<R>, Result<(Frame, Option<Vec<u8>>)>) {
    let res = codec::read_frame(&mut rx).await;
    (rx, res)
}

/// Generic over the AsyncRead/AsyncWrite halves so both transports share
/// the same dispatch loop. `expected_token` is set for TCP and `None` for
/// local sockets, whose access check happened at the socket path.
async fn handle_connection<R, W>(
    rx: R,
    mut tx: W,
    session: Session,
    expected_token: Arc<Option<String>>,
    mathjax: MathJax,
    pluto: Pluto,
    files_mode: Arc<FilesMode>,
    // Singleton handles retained on the call chain for backward compat
    // and to keep the run_local / run_tcp signatures unchanged. All op
    // handlers now route through Workspaces per ADR 0014; these
    // bindings are dead in `handle_connection` itself.
    #[allow(unused_variables, dead_code)] kernel: Kernel,
    #[allow(unused_variables, dead_code)] concept: Arc<ConceptStore>,
    #[allow(unused_variables, dead_code)] repl: Repl,
    watcher: Option<Arc<Watcher>>,
    label: Arc<Option<String>>,
    workspaces: Workspaces,
    ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    agent_events_tx: broadcast::Sender<AgentMessage>,
    fe_command_tx: broadcast::Sender<FeCommandEvt>,
    repl_frame_tx: broadcast::Sender<ReplFrameMsg>,
    clients: Clients,
    transport: &'static str,
    peer: Option<String>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // CANCEL-SAFETY: `read_frame` is NOT cancellation-safe (it reads the `\n`
    // envelope then `read_exact`s the blob tail across two awaits). Polling
    // `codec::read_frame(&mut rx)` directly as a select! arm meant that when
    // another arm (pty bytes / watcher change) completed mid-blob, select!
    // dropped the half-read future: the envelope was consumed but the blob
    // tail was not, so the next read parsed leftover binary as a JSON envelope
    // and failed, dropping the connection. Hold one read future across loop
    // iterations and poll it by `&mut`, so a cancelled select! pauses it (it
    // resumes mid-blob next iteration). The future owns the reader (via
    // `read_owned`) and hands it back on completion. Mirrors frontend 8746b74;
    // lower-risk here (frontend→backend reqs rarely carry blob tails) but the
    // same latent footgun. Flagged from the Windows side 2026-05-26.
    let mut read_fut = Some(Box::pin(read_owned(codec::buffered(rx))));
    tracing::debug!(transport, "connection ready");

    // Per-connection pty for the LLM pane. Lazy: only spawned when
    // the frontend sends `pty.open`. When `Some`, the select loop
    // multiplexes wire reads with pty byte output so the terminal
    // streams live without polling.
    let mut pty: Option<Pty> = None;
    let b64 = base64::engine::general_purpose::STANDARD;

    // Connected-client registry entry (ADR 0010/0013). Registered on the
    // first `hello` (when this connection's client_id is known) and held
    // for the connection's lifetime; the guard deregisters on any exit
    // path (clean EOF, error, task drop). `None` until hello arrives.
    let mut client_guard: Option<crate::clients::ClientGuard> = None;

    // Per-connection auth state (ADR 0010 hardening). The token gate on `hello`
    // is not sufficient on its own: nothing forces a client to send hello, and
    // the dispatch loop below serves file.read / repl.eval / file.download /
    // agent.send with no handshake — so a token-configured backend was still
    // fully reachable by simply skipping hello. Starts `true` ONLY in open-config
    // mode (no token configured); when a token IS configured it starts `false`
    // and flips to `true` only on a hello whose token matches. Every non-hello
    // op is gated on this flag.
    let mut authenticated = expected_token.is_none();

    // One file-watcher subscription per connection. Each connection writes
    // its own preview.changed evt frame; the broadcast channel's per-
    // receiver lag detection lets us notice and log if this connection is
    // falling behind editor saves.
    let mut watcher_rx = watcher.as_ref().map(|w| w.subscribe());

    // One workspace-lifecycle subscription per connection. Always present
    // (the channel is created unconditionally in `run`), unlike the file
    // watcher which can fail to start. Each connection writes its own
    // `workspace.changed` evt frame; the broadcast's per-receiver lag
    // detection surfaces a connection that fell behind.
    let mut ws_events_rx = ws_events_tx.subscribe();

    // One agent-relay subscription per connection. Like the workspace bus
    // it's always present (channel created unconditionally in `run`). Each
    // connection writes its own `agent.message` evt frame; the broadcast's
    // per-receiver lag detection surfaces a connection that fell behind.
    let mut agent_events_rx = agent_events_tx.subscribe();

    // One FE-command subscription per connection (ADR 0025). Like the agent
    // bus it's always present (channel created unconditionally in `run`). Each
    // connection writes its own `fe.command` evt frame; the broadcast's
    // per-receiver lag detection surfaces a connection that fell behind.
    let mut fe_command_rx = fe_command_tx.subscribe();

    // One REPL-frame subscription per connection. Streamed eval frames arrive
    // here (published by the per-workspace REPL supervisor); each connection
    // writes its own `repl.frame` evt frame. Like the agent bus, always
    // present; the broadcast's per-receiver lag detection surfaces a
    // connection that fell behind a fast-printing eval.
    let mut repl_frame_rx = repl_frame_tx.subscribe();

    // Monitor tick bus (ADR 0020). Sampling is always-on; this connection only
    // forwards ticks while its drawer is open — `monitor.subscribe` flips the
    // flag, `monitor.unsubscribe` clears it. The receiver is still polled while
    // unsubscribed so it never lags (the client gets fresh ticks on subscribe).
    // Optional like `watcher_rx`: `None` if the hub wasn't installed.
    let mut monitor_rx = workspaces.monitor_hub().map(|h| h.subscribe());
    let mut monitor_subscribed = false;

    loop {
        // tokio::select! can't borrow `pty` mutably and immutably at
        // once across arms, so split based on whether the pty is up.
        let frame = if let Some(p) = pty.as_mut() {
            tokio::select! {
                biased;
                done = read_fut.as_mut().expect("read_fut is always Some at loop top") => {
                    // Completed: reclaim the reader and arm the next read.
                    let (rx_back, wire) = done;
                    read_fut = Some(Box::pin(read_owned(rx_back)));
                    match wire {
                        Ok((f, _blob)) => f,
                        Err(e) => {
                            tracing::debug!(error = %e, transport, "read_frame returned; closing");
                            return Ok(());
                        }
                    }
                }
                bytes = p.rx.recv() => {
                    let Some(bytes) = bytes else {
                        // pty closed (tmux exited); drop it and
                        // continue serving the rest of the protocol.
                        tracing::info!(transport, "pty reader EOF; dropping pty");
                        pty = None;
                        continue;
                    };
                    // Attribute this chunk to the latest resize burst
                    // (if a window is open) for the 18:17Z timing ask.
                    p.note_outgoing_bytes(bytes.len());
                    // Auth gate (ADR 0010 hardening): never push evt frames to an
                    // unauthenticated connection — a bad re-hello can flip
                    // `authenticated` back to false while the pty stays open, and
                    // without this gate terminal output would keep flowing anyway.
                    if authenticated {
                        let payload = serde_json::json!({ "data_b64": b64.encode(&bytes) });
                        let evt = Frame::evt(op::PTY_EVT, payload);
                        write_frame_to(&mut tx, &evt, None).await?;
                    }
                    continue;
                }
                change = recv_watcher(&mut watcher_rx) => {
                    if authenticated {
                        write_preview_changed(&mut tx, change, transport).await?;
                    }
                    continue;
                }
                wsc = recv_ws_events(&mut ws_events_rx) => {
                    // Auth gate (ADR 0010 hardening): never push evt frames to an
                    // unauthenticated connection. Drain the channel, drop the frame.
                    if authenticated {
                        write_workspace_changed(&mut tx, wsc, transport).await?;
                    }
                    continue;
                }
                msg = recv_agent_msg(&mut agent_events_rx) => {
                    if authenticated {
                        write_agent_message(&mut tx, msg, transport).await?;
                    }
                    continue;
                }
                fc = recv_fe_command(&mut fe_command_rx) => {
                    if authenticated {
                        write_fe_command(&mut tx, fc, transport).await?;
                    }
                    continue;
                }
                rf = recv_repl_frame(&mut repl_frame_rx) => {
                    if authenticated {
                        write_repl_frame(&mut tx, rf, transport).await?;
                    }
                    continue;
                }
                tick = recv_monitor(&mut monitor_rx) => {
                    if authenticated && monitor_subscribed {
                        write_monitor_tick(&mut tx, tick, transport).await?;
                    }
                    continue;
                }
            }
        } else {
            tokio::select! {
                biased;
                done = read_fut.as_mut().expect("read_fut is always Some at loop top") => {
                    let (rx_back, wire) = done;
                    read_fut = Some(Box::pin(read_owned(rx_back)));
                    match wire {
                        Ok((f, _blob)) => f,
                        Err(e) => {
                            tracing::debug!(error = %e, transport, "read_frame returned; closing");
                            return Ok(());
                        }
                    }
                }
                change = recv_watcher(&mut watcher_rx) => {
                    if authenticated {
                        write_preview_changed(&mut tx, change, transport).await?;
                    }
                    continue;
                }
                wsc = recv_ws_events(&mut ws_events_rx) => {
                    // Auth gate (ADR 0010 hardening): never push evt frames to an
                    // unauthenticated connection. Drain the channel, drop the frame.
                    if authenticated {
                        write_workspace_changed(&mut tx, wsc, transport).await?;
                    }
                    continue;
                }
                msg = recv_agent_msg(&mut agent_events_rx) => {
                    if authenticated {
                        write_agent_message(&mut tx, msg, transport).await?;
                    }
                    continue;
                }
                fc = recv_fe_command(&mut fe_command_rx) => {
                    if authenticated {
                        write_fe_command(&mut tx, fc, transport).await?;
                    }
                    continue;
                }
                rf = recv_repl_frame(&mut repl_frame_rx) => {
                    if authenticated {
                        write_repl_frame(&mut tx, rf, transport).await?;
                    }
                    continue;
                }
                tick = recv_monitor(&mut monitor_rx) => {
                    if authenticated && monitor_subscribed {
                        write_monitor_tick(&mut tx, tick, transport).await?;
                    }
                    continue;
                }
            }
        };

        if frame.kind != Kind::Req {
            tracing::debug!(?frame.kind, op = %frame.op, transport, "ignoring non-req frame");
            continue;
        }

        // Auth gate (ADR 0010 hardening). With a token configured, every op
        // except `hello` requires a prior token-valid hello on THIS connection.
        // Without this, the token is trivially bypassable: a client skips the
        // handshake and calls file.read / repl.eval / file.download / agent.send
        // directly, and the dispatch loop below serves them regardless.
        if !authenticated && frame.op.as_str() != op::HELLO {
            tracing::warn!(op = %frame.op, ?peer, "op rejected: unauthenticated (no token-valid hello)");
            let payload = serde_json::json!({
                "error": "authentication required: send a token-valid hello first",
                "code": "unauthenticated",
            });
            write_frame_to(&mut tx, &Frame::res(frame.id, &frame.op, payload), None).await?;
            continue;
        }

        let out_frames = match frame.op.as_str() {
            op::HELLO => {
                // Register this connection in the client roster the first
                // time we learn its client_id (a reconnect re-sends hello
                // on the same connection — keep the original guard). Done
                // before `handle_hello` so `clients_connected` counts self.
                if client_guard.is_none() {
                    if let Ok(req) =
                        serde_json::from_value::<sot_protocol::HelloReq>(frame.payload.clone())
                    {
                        client_guard =
                            Some(clients.register(req.client_id, transport, peer.clone()));
                    }
                }
                // Flip the per-connection auth flag based on THIS hello's token
                // (recomputed on every hello so a reconnect re-auths). Open-config
                // mode has `expected_token == None`, so this stays true. Mirrors
                // the same check `handle_hello` uses to shape its response frame.
                authenticated = match expected_token.as_deref() {
                    None => true,
                    Some(expected) => {
                        let presented =
                            serde_json::from_value::<sot_protocol::HelloReq>(frame.payload.clone())
                                .ok()
                                .and_then(|r| r.token)
                                .unwrap_or_default();
                        handlers::constant_time_eq(presented.as_bytes(), expected.as_bytes())
                    }
                };
                handlers::handle_hello(
                    frame.id,
                    frame.payload,
                    &session,
                    expected_token.as_ref(),
                    &files_mode,
                    label.as_deref(),
                    &clients,
                )
                .await?
            }
            op::TREE_ROOT => {
                handlers::handle_tree_root(frame.id, frame.payload, &session, &workspaces).await?
            }
            op::TREE_CHILDREN => {
                handlers::handle_tree_children(frame.id, frame.payload, &session, &workspaces)
                    .await?
            }
            op::NAV_TOGGLE_HIDDEN => {
                handlers::handle_nav_toggle_hidden(frame.id, frame.payload, &session, &workspaces)
                    .await?
            }
            op::PREVIEW_GET => {
                handlers::handle_preview_get(frame.id, frame.payload, &session, &workspaces).await?
            }
            op::IMAGE_CROP => {
                handlers::handle_image_crop(frame.id, frame.payload, &session, &workspaces).await?
            }
            op::MATH_RENDER => {
                handlers::handle_math_render(frame.id, frame.payload, &session, &mathjax).await?
            }
            op::PLUTO_OPEN => {
                handlers::handle_pluto_open(frame.id, frame.payload, &session, &pluto, &workspaces)
                    .await?
            }
            op::VIDEO_OPEN => {
                handlers::handle_video_open(frame.id, frame.payload, &session).await?
            }
            op::DOCS_OPEN => {
                // Per-connection site root (ADR 0029): this connection's serial
                // selects/owns its docs-map entry and becomes the URL's first path
                // segment. `None` only before hello registers the guard, which
                // always precedes docs.open in practice.
                let serial = client_guard.as_ref().map(|g| g.serial());
                handlers::handle_docs_open(frame.id, frame.payload, &session, serial, &workspaces)
                    .await?
            }
            op::QUARTO_OPEN => {
                handlers::handle_quarto_open(frame.id, frame.payload, &session).await?
            }
            op::FILE_UPLOAD => handlers::handle_file_upload(frame.id, frame.payload).await?,
            op::FILE_DOWNLOAD => {
                // Streams chunk frames straight to the socket (bounded memory),
                // so it writes its own frames and skips the response-write below.
                handlers::stream_file_download(&mut tx, frame.id, frame.payload).await?;
                continue;
            }
            op::KERNEL_REQUEST => {
                handlers::handle_kernel_request(frame.id, frame.payload, &session, &workspaces)
                    .await?
            }
            op::CONCEPT_READ => {
                handlers::handle_concept_read(frame.id, frame.payload, &session, &workspaces)
                    .await?
            }
            op::CONCEPT_WRITE => {
                handlers::handle_concept_write(frame.id, frame.payload, &session, &workspaces)
                    .await?
            }
            op::CONCEPT_LIST => {
                handlers::handle_concept_list(frame.id, frame.payload, &session, &workspaces)
                    .await?
            }
            op::FILE_READ => {
                handlers::handle_file_read(frame.id, frame.payload, &session, &workspaces).await?
            }
            op::FILE_WRITE => {
                handlers::handle_file_write(frame.id, frame.payload, &session, &workspaces).await?
            }
            op::FILE_DELETE => {
                handlers::handle_file_delete(frame.id, frame.payload, &session, &workspaces).await?
            }
            op::REPL_EVAL => {
                handlers::handle_repl_eval(frame.id, frame.payload, &session, &workspaces).await?
            }
            op::REPL_RUN_FILE => {
                handlers::handle_repl_run_file(frame.id, frame.payload, &session, &workspaces)
                    .await?
            }
            op::REPL_INTERRUPT => {
                handlers::handle_repl_interrupt(frame.id, frame.payload, &session, &workspaces)
                    .await?
            }
            op::TMUX_LIST_SESSIONS => {
                handlers::handle_tmux_list_sessions(frame.id, frame.payload, &session).await?
            }
            op::TMUX_LIST_PANES => {
                handlers::handle_tmux_list_panes(frame.id, frame.payload, &session).await?
            }
            op::TMUX_CREATE_SESSION => {
                handlers::handle_tmux_create_session(frame.id, frame.payload, &session).await?
            }
            op::TMUX_KILL_SESSION => {
                handlers::handle_tmux_kill_session(frame.id, frame.payload, &session).await?
            }
            op::TMUX_CAPTURE_PANE => {
                handlers::handle_tmux_capture_pane(frame.id, frame.payload, &session).await?
            }
            op::DIRECTORY_LIST => {
                handlers::handle_directory_list(frame.id, frame.payload, &session).await?
            }
            op::WORKSPACE_CREATE => {
                handlers::handle_workspace_create(
                    frame.id,
                    frame.payload,
                    &session,
                    &workspaces,
                    &ws_events_tx,
                )
                .await?
            }
            op::WORKSPACE_LIST => {
                handlers::handle_workspace_list(frame.id, frame.payload, &workspaces).await?
            }
            op::AGENT_SEND => {
                handlers::handle_agent_send(frame.id, frame.payload, &agent_events_tx).await?
            }
            op::FE_COMMAND_SEND => {
                handlers::handle_fe_command_send(frame.id, frame.payload, &fe_command_tx).await?
            }
            op::UPDATE_CHECK => crate::update::handle_update_check(frame.id).await?,
            op::WORKSPACE_DESTROY => {
                handlers::handle_workspace_destroy(
                    frame.id,
                    frame.payload,
                    &session,
                    &workspaces,
                    &ws_events_tx,
                )
                .await?
            }
            op::PTY_OPEN => {
                let req: PtyOpenReq = match serde_json::from_value(frame.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        let payload = serde_json::json!({
                            "error": format!("pty.open payload: {e}"),
                            "code": "bad_request",
                        });
                        write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                            .await?;
                        continue;
                    }
                };
                // Name validation (security review): an explicit `target`
                // becomes the real tmux session name — a `|`-containing one
                // would corrupt `tmux.rs`'s naive `|`-delimited
                // `list-sessions`/`list-panes` parsing for every session, not
                // just this one. `None` (the default target) is exempt: it's
                // the hardcoded `DEFAULT_TMUX_TARGET` constant, not
                // request-controlled.
                if let Some(t) = req.target.as_deref() {
                    if !handlers::valid_name(t) {
                        let payload = serde_json::json!({
                            "error": format!(
                                "invalid target {t:?} (want 1-64 chars of [A-Za-z0-9._-])"
                            ),
                            "code": "bad_target",
                        });
                        write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                            .await?;
                        continue;
                    }
                }
                let requested_target = req
                    .target
                    .as_deref()
                    .unwrap_or(crate::pty::DEFAULT_TMUX_TARGET);
                // Root the tmux session at the owning workspace's project
                // dir (`-c <project_root>`). Without it `new-session -A`
                // creates the session in the daemon's launch dir (usually
                // `$HOME`), so the orchestrator's shell — and its trust
                // scope — land on the wrong workspace. `None` for the
                // home-base default (no matching workspace session).
                let requested_cwd = workspaces.project_root_for_tmux(requested_target);
                // Workspace slug for the same session — stamped into the
                // spawned env as SOT_WORKSPACE so a session in the pane
                // can gate FE nav commands on its workspace. `None` for the
                // home-base default (no matching workspace).
                let requested_slug = workspaces.slug_for_tmux(requested_target);
                if let Some(existing) = pty.as_ref() {
                    // Same target → just resize, keep the existing pty.
                    // Different target (Sessions-mode re-attach, ADR 0013)
                    // → drop the existing pty so the loop spawns fresh
                    // against the new target below.
                    if existing.target() == requested_target {
                        if let Err(e) = existing.resize(req.cols, req.rows) {
                            tracing::warn!(error = %e, "pty resize failed");
                        }
                        let res = PtyOpenRes {
                            cols: req.cols,
                            rows: req.rows,
                            // Same-target resize keeps the existing pty and
                            // never re-arms autostart, so no probe needed.
                            pane_command: None,
                        };
                        vec![(
                            Frame::res(frame.id, op::PTY_OPEN, serde_json::to_value(res)?),
                            None,
                        )]
                    } else if req.user_switch {
                        tracing::info!(
                            from = %existing.target(),
                            to = %requested_target,
                            peer = ?peer,
                            user_switch = req.user_switch,
                            "pty re-target: dropping existing pty for fresh spawn"
                        );
                        // Release the old session cleanly BEFORE dropping the
                        // Pty: flag the reader as deliberate-teardown (no EOF
                        // respawn) and detach our client so tmux keeps the
                        // session alive for a clean re-attach. Without this the
                        // bare `pty = None` closed the master fd abruptly while
                        // our client was still attached, and the rapid
                        // re-target case raced tmux into destroying the
                        // left-behind (clientless) session — a bare-shell
                        // workspace lost its bash on every switch-away.
                        existing.shutdown();
                        pty = None;
                        match Pty::spawn(
                            req.cols,
                            req.rows,
                            Some(requested_target),
                            requested_cwd.as_deref(),
                            requested_slug.as_deref(),
                        ) {
                            Ok(p) => {
                                tracing::info!(
                                    cols = req.cols,
                                    rows = req.rows,
                                    target = %requested_target,
                                    "pty.open spawned tmux (re-target)"
                                );
                                pty = Some(p);
                                let res = PtyOpenRes {
                                    cols: req.cols,
                                    rows: req.rows,
                                    // Authoritative "is claude already up in
                                    // this pane?" signal for the FE autostart
                                    // guard. Queried post-spawn so the session
                                    // exists. (prompt-spam fix.)
                                    pane_command: crate::tmux::TmuxClient::new()
                                        .active_pane_command(requested_target),
                                };
                                vec![(
                                    Frame::res(frame.id, op::PTY_OPEN, serde_json::to_value(res)?),
                                    None,
                                )]
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "pty re-target spawn failed");
                                let payload = serde_json::json!({
                                    "error": format!("{e:#}"),
                                    "code": "pty_spawn_failed",
                                });
                                vec![(Frame::res(frame.id, op::PTY_OPEN, payload), None)]
                            }
                        }
                    } else {
                        // #5 single-pty guard (ADR-0014): a pty.open for a
                        // DIFFERENT target that is NOT an explicit user
                        // workspace-switch (a roaming FE's background re-attach,
                        // a daemon-boot open, etc.) must NOT yank the single
                        // foreground pty — that ping-pong between two FEs + the
                        // boot-pty froze create-session for ~1min. Keep the
                        // existing pty untouched; only `user_switch == true` (the
                        // FE sets it at switch_to_workspace) re-targets.
                        tracing::info!(
                            from = %existing.target(),
                            to = %requested_target,
                            peer = ?peer,
                            user_switch = req.user_switch,
                            "pty re-target SUPPRESSED — not a user switch; keeping foreground"
                        );
                        let res = PtyOpenRes {
                            cols: req.cols,
                            rows: req.rows,
                            pane_command: None,
                        };
                        vec![(
                            Frame::res(frame.id, op::PTY_OPEN, serde_json::to_value(res)?),
                            None,
                        )]
                    }
                } else {
                    match Pty::spawn(
                        req.cols,
                        req.rows,
                        Some(requested_target),
                        requested_cwd.as_deref(),
                        requested_slug.as_deref(),
                    ) {
                        Ok(p) => {
                            tracing::info!(
                                cols = req.cols,
                                rows = req.rows,
                                target = %requested_target,
                                "pty.open spawned tmux"
                            );
                            pty = Some(p);
                            let res = PtyOpenRes {
                                cols: req.cols,
                                rows: req.rows,
                                // Same authoritative claude-already-running
                                // probe as the re-target branch.
                                pane_command: crate::tmux::TmuxClient::new()
                                    .active_pane_command(requested_target),
                            };
                            vec![(
                                Frame::res(frame.id, op::PTY_OPEN, serde_json::to_value(res)?),
                                None,
                            )]
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "pty spawn failed");
                            let payload = serde_json::json!({
                                "error": format!("{e:#}"),
                                "code": "pty_spawn_failed",
                            });
                            vec![(Frame::res(frame.id, op::PTY_OPEN, payload), None)]
                        }
                    }
                }
            }
            op::PTY_RESIZE => {
                let req: PtyResizeReq = match serde_json::from_value(frame.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "pty.resize payload parse failed");
                        continue;
                    }
                };
                if let Some(p) = pty.as_ref() {
                    if let Err(e) = p.resize(req.cols, req.rows) {
                        tracing::warn!(error = %e, "pty resize failed");
                    }
                }
                continue; // no response for resize
            }
            op::PTY_WRITE => {
                let req: PtyWriteReq = match serde_json::from_value(frame.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "pty.write payload parse failed");
                        continue;
                    }
                };
                if let Some(p) = pty.as_ref() {
                    match b64.decode(req.data_b64.as_bytes()) {
                        Ok(bytes) => {
                            if let Err(e) = p.write(&bytes) {
                                tracing::warn!(error = %e, "pty write failed");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "pty.write b64 decode failed");
                        }
                    }
                }
                continue; // no response for write
            }
            op::MONITOR_SUBSCRIBE => {
                // Open this connection's live tick delivery (sampling is
                // already running). Reply with the host roster + base cadence
                // so the frontend can lay out panels before the first tick.
                monitor_subscribed = true;
                let hosts = workspaces
                    .monitor_hub()
                    .map(|h| h.host_names())
                    .unwrap_or_default();
                let res = MonitorSubscribeRes {
                    interval_s: 1.0,
                    hosts,
                };
                vec![(
                    Frame::res(frame.id, op::MONITOR_SUBSCRIBE, serde_json::to_value(res)?),
                    None,
                )]
            }
            op::MONITOR_UNSUBSCRIBE => {
                monitor_subscribed = false;
                vec![(
                    Frame::res(frame.id, op::MONITOR_UNSUBSCRIBE, serde_json::json!({})),
                    None,
                )]
            }
            op::MONITOR_HISTORY => {
                let req: MonitorHistoryReq =
                    serde_json::from_value(frame.payload).context("monitor.history payload")?;
                let hosts = workspaces
                    .monitor_hub()
                    .map(|h| h.history(&req))
                    .unwrap_or_default();
                let res = MonitorHistoryRes { hosts };
                vec![(
                    Frame::res(frame.id, op::MONITOR_HISTORY, serde_json::to_value(res)?),
                    None,
                )]
            }
            other => {
                tracing::warn!(op = %other, transport, "unknown op");
                let payload = serde_json::json!({ "error": format!("unknown op: {other}") });
                vec![(Frame::res(frame.id, other, payload), None)]
            }
        };

        for (out_frame, out_blob) in out_frames {
            write_frame_to(&mut tx, &out_frame, out_blob.as_deref()).await?;
        }
    }
}

/// Awaits the next file-change from the watcher subscription. When the
/// connection has no watcher (Watcher::spawn failed at startup) this future
/// stays pending forever, leaving the `tokio::select!` arm inactive.
async fn recv_watcher(
    rx: &mut Option<broadcast::Receiver<PreviewChanged>>,
) -> Result<PreviewChanged, broadcast::error::RecvError> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// Translates one watcher event into a `preview.changed` evt frame on the
/// wire. Returns `Ok(true)` if a frame was written, `Ok(false)` if the
/// receiver was lagged or closed (skip and keep the connection alive).
async fn write_preview_changed<W>(
    tx: &mut W,
    change: Result<PreviewChanged, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match change {
        Ok(c) => {
            let payload = serde_json::json!({
                "path": c.path.to_string_lossy(),
                "node_id": c.node_id,
                "kind": c.kind.as_str(),
            });
            let frame = Frame::evt(op::PREVIEW_CHANGED, payload).with_rev(c.revision);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "preview watcher lagged on this connection; client missed file events"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "preview watcher channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next workspace lifecycle event. The channel is always present
/// (created unconditionally in `run`), so unlike `recv_watcher` this takes a
/// plain receiver rather than an `Option`.
async fn recv_ws_events(
    rx: &mut broadcast::Receiver<WorkspaceChanged>,
) -> Result<WorkspaceChanged, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one workspace lifecycle event into a `workspace.changed` evt
/// frame on the wire. Returns `Ok(true)` if a frame was written, `Ok(false)`
/// if the receiver was lagged or closed (skip and keep the connection alive).
async fn write_workspace_changed<W>(
    tx: &mut W,
    change: Result<WorkspaceChanged, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match change {
        Ok(c) => {
            let payload = serde_json::json!({
                "action": c.action,
                "slug": c.slug,
                "workspace_id": c.workspace_id,
            });
            let frame = Frame::evt(op::WORKSPACE_CHANGED, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "workspace event bus lagged on this connection; client missed workspace changes"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "workspace event bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next relayed agent message. The channel is always present
/// (created unconditionally in `run`), so like `recv_ws_events` this takes a
/// plain receiver rather than an `Option`.
async fn recv_agent_msg(
    rx: &mut broadcast::Receiver<AgentMessage>,
) -> Result<AgentMessage, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one relayed agent message into an `agent.message` evt frame on
/// the wire. Returns `Ok(true)` if a frame was written, `Ok(false)` if the
/// receiver was lagged or closed (skip and keep the connection alive).
async fn write_agent_message<W>(
    tx: &mut W,
    msg: Result<AgentMessage, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            let payload = serde_json::json!({
                "from": m.from,
                "to": m.to,
                "text": m.text,
                "ts": m.ts,
            });
            let frame = Frame::evt(op::AGENT_MESSAGE, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "agent relay bus lagged on this connection; client missed messages"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "agent relay bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next FE command (ADR 0025). The channel is always present
/// (created unconditionally in `run`), so like `recv_agent_msg` this takes a
/// plain receiver rather than an `Option`.
async fn recv_fe_command(
    rx: &mut broadcast::Receiver<FeCommandEvt>,
) -> Result<FeCommandEvt, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one FE command into an `fe.command` evt frame on the wire
/// (ADR 0025). Returns `Ok(true)` if a frame was written, `Ok(false)` if the
/// receiver was lagged or closed (skip and keep the connection alive). The
/// evt is broadcast to every connection; the FE self-filters on `target`.
async fn write_fe_command<W>(
    tx: &mut W,
    evt: Result<FeCommandEvt, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match evt {
        Ok(e) => {
            let frame = Frame::evt(op::FE_COMMAND, serde_json::to_value(e)?);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "fe command bus lagged on this connection; client missed commands"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "fe command bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next streamed REPL frame. The channel is always present (created
/// unconditionally in `run`), so like `recv_agent_msg` this takes a plain
/// receiver rather than an `Option`.
async fn recv_repl_frame(
    rx: &mut broadcast::Receiver<ReplFrameMsg>,
) -> Result<ReplFrameMsg, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one streamed REPL frame into a `repl.frame` evt frame on the
/// wire. Returns `Ok(true)` if a frame was written, `Ok(false)` if the
/// receiver was lagged or closed (skip and keep the connection alive). The
/// `frame` value is passed through verbatim — its `{kind, ...}` shape is
/// kernel-defined, so the backend stays oblivious to new frame kinds.
async fn write_repl_frame<W>(
    tx: &mut W,
    msg: Result<ReplFrameMsg, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            let payload = serde_json::json!({
                "eval_id": m.eval_id,
                "workspace_id": m.workspace_id,
                "frame": m.frame,
            });
            let frame = Frame::evt(op::REPL_FRAME, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "repl frame bus lagged on this connection; client missed frames"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "repl frame bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next monitor tick. Mirrors `recv_watcher`: when the hub wasn't
/// installed the receiver is `None` and this stays pending, leaving the
/// select! arm inactive.
async fn recv_monitor(
    rx: &mut Option<broadcast::Receiver<HostLatest>>,
) -> Result<HostLatest, broadcast::error::RecvError> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// Translates one monitor tick into a `monitor.tick` evt (one host per evt; the
/// frontend merges by host). Skips on lag/close, keeping the connection alive.
async fn write_monitor_tick<W>(
    tx: &mut W,
    msg: Result<HostLatest, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            let evt = MonitorTickEvt { hosts: vec![m] };
            let frame = Frame::evt(op::MONITOR_TICK, serde_json::to_value(evt)?);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "monitor bus lagged on this connection"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "monitor bus channel closed");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pane_activity;

    #[tokio::test]
    async fn write_frame_within_times_out_on_stuck_peer() {
        // The reaper's core: a peer whose socket buffer is full because it
        // stopped draining (the half-open / Recv-Q stall we hit) must not park
        // the connection task forever — the bounded write trips and errors so
        // `handle_connection` drops the connection.
        use super::write_frame_within;
        use sot_protocol::Frame;
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use tokio::io::AsyncWrite;

        struct StuckWriter;
        impl AsyncWrite for StuckWriter {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                Poll::Pending
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
        }

        let frame = Frame::evt("test.stall", serde_json::json!({"k": "v"}));
        let res = write_frame_within(
            &mut StuckWriter,
            &frame,
            None,
            std::time::Duration::from_millis(50),
        )
        .await;
        assert!(
            res.is_err(),
            "a non-draining peer must trip the write timeout"
        );
        assert!(res.unwrap_err().to_string().contains("not draining"));
    }

    #[tokio::test]
    async fn write_frame_within_succeeds_on_healthy_peer() {
        // The complement: a sink that drains instantly never trips the timeout,
        // so the reaper can't false-drop a healthy connection.
        use super::write_frame_within;
        use sot_protocol::Frame;

        let mut sink: Vec<u8> = Vec::new();
        let frame = Frame::evt("test.ok", serde_json::json!({"k": "v"}));
        let res =
            write_frame_within(&mut sink, &frame, None, std::time::Duration::from_secs(5)).await;
        assert!(res.is_ok(), "a healthy peer must not trip the timeout");
        assert!(!sink.is_empty(), "frame bytes should have been written");
    }

    #[test]
    fn write_deadline_scales_with_blob_size() {
        // Regression for the false-drop of a legit large preview blob: the
        // deadline must be the tight floor for small/no-blob frames (reaper stays
        // sharp) and grow proportionally for a bulk blob so a draining 71 MB
        // render isn't reaped mid-write.
        use super::{write_deadline, MIN_BLOB_DRAIN_RATE, WRITE_TIMEOUT};
        assert_eq!(write_deadline(None), WRITE_TIMEOUT, "no blob → floor");
        let small = vec![0u8; MIN_BLOB_DRAIN_RATE as usize - 1];
        assert_eq!(
            write_deadline(Some(&small)),
            WRITE_TIMEOUT,
            "sub-rate blob → floor (no grace yet)"
        );
        let big = vec![0u8; 5 * MIN_BLOB_DRAIN_RATE as usize];
        assert_eq!(
            write_deadline(Some(&big)),
            WRITE_TIMEOUT + std::time::Duration::from_secs(5),
            "deadline = floor + size / MIN_BLOB_DRAIN_RATE"
        );
    }

    // Realistic footers captured from live agents (input box + status line +
    // permission hint). The working one adds the live generation status line.
    const IDLE_FOOTER: &str = "──────────────────\n❯ \n──────────────────\n  Opus 4 [abc] ·think:max | v2.1.177 | Ship of Tools:main | 0 uncommitted\n  Session: 155k (in:155k out:3) | $253.27\n  ⏵⏵ bypass permissions on · 1 monitor";
    const WORKING_FOOTER: &str = "● Bash(echo hi)\n  ⎿  Running…\n\n✢ Ionizing… (2m 55s · ↓ 12.3k tokens)\n\n──────────────────\n❯ \n──────────────────\n  Session: 155k (in:155k out:3) | $253.27\n  ⏵⏵ bypass permissions on · 1 monitor";

    #[test]
    fn no_claude_marker_is_empty() {
        // A bare shell / unrelated pane → no state (FE falls back to registry).
        assert_eq!(pane_activity(""), "");
        assert_eq!(pane_activity("user@host:~$ ls -la\nfoo bar baz\n"), "");
        assert_eq!(pane_activity("julia> 1 + 1\n2\n"), "");
    }

    #[test]
    fn idle_footer_is_idle() {
        // claude up at the prompt — footer present, no running timer.
        assert_eq!(pane_activity(IDLE_FOOTER), "idle");
    }

    #[test]
    fn running_timer_means_working() {
        // Current builds: the parenthesised elapsed timer is the generating signal.
        assert_eq!(pane_activity(WORKING_FOOTER), "working");
        // Older builds: "(esc to interrupt)" inside the status line.
        assert_eq!(
            pane_activity("? for shortcuts\n✻ Working… (12s · esc to interrupt)"),
            "working"
        );
    }

    #[test]
    fn prose_quoting_chrome_is_not_working() {
        // Contamination immunity: an agent whose OUTPUT quotes the footer — the
        // bare phrase AND the timer format — must not read working, because prose
        // never *starts a line* with a spinner glyph.
        let s = format!(
            "note: esc to interrupt; the live timer reads (2m 55s · 12.3k tokens)\n{IDLE_FOOTER}"
        );
        assert_eq!(pane_activity(&s), "idle");
    }

    #[test]
    fn working_status_above_a_todo_list_is_working() {
        // The myanalysis regression: a working agent with a todo list between
        // its status line and the input box. The status line sits ~15 lines up,
        // past any fixed footer window — scanning the whole capture catches it.
        let s = format!(
            "✽ Wiring weakdep extensions… (3m 16s · ↓ 14.8k tokens)\n  \u{23bf}  \u{25fc} Wire FeaturePrep\n     \u{25fc} Land MyDetector\n     \u{25fb} Validate pipeline\n     \u{2714} Verify DL stack\n     \u{2714} Land FeaturePrep\n\n{IDLE_FOOTER}"
        );
        assert_eq!(pane_activity(&s), "working");
    }

    #[test]
    fn post_turn_summary_is_idle() {
        // The completed-turn line is spinner-led but has no parenthesised timer
        // ("Churned for 17m 59s", not "(17m"), so it reads idle, not working.
        let s = format!("✻ Churned for 17m 59s · 1 shell, 1 monitor still running\n{IDLE_FOOTER}");
        assert_eq!(pane_activity(&s), "idle");
    }
}
