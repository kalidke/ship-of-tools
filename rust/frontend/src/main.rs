// sot-frontend
//
// Native local window. Owns rendering end-to-end. No terminal protocols.
// See ADR 0003 (rendering surface), ADR 0011 (rendering split — ratatui chrome
// vs. preview-layer), ADR 0012 (frontend stack).
//
// The transport task is spawned on a dedicated tokio runtime alongside winit.
// winit drives the main thread for window/input/redraw; tokio carries the
// Unix-socket protocol traffic to/from the backend (ADR 0010).

mod chrome;
mod cli;
mod download;
mod edit_buffer;
mod gpu;
mod hosts;
mod keybindings;
mod layout;
mod monitor_view;
mod paths;
mod preview;
mod proxy_listen;
mod selfupdate;
mod settings;
mod state;
mod state_persistence;
mod term;
mod text;
mod transport;

use std::sync::mpsc;

use anyhow::Result;
use winit::event_loop::{ControlFlow, EventLoop};

/// Windows taskbar grouping: declare an explicit Application User Model ID so
/// the running window merges into the *pinned* taskbar button instead of
/// spawning a second one. Without this the window groups by its exe identity
/// (`sot.exe`, run from a staged copy under %LOCALAPPDATA%), while the pinned
/// shortcut launches `powershell.exe` (the launcher) and resolves to *that*
/// identity — two different AUMIDs, so Windows shows a separate button. The
/// SAME id must be set on the shortcut's `System.AppUserModel.ID` property
/// (see `scripts/install-shortcut.ps1`). Must run before any window exists.
/// Non-fatal: a failure just degrades to the old (ungrouped) behaviour.
#[cfg(windows)]
const APP_USER_MODEL_ID: &str = "ShipOfTools.Sot";

#[cfg(windows)]
fn set_app_user_model_id() {
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    let wide: Vec<u16> = APP_USER_MODEL_ID
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 buffer that outlives the
    // call; the API copies the string. The HRESULT is ignored on purpose.
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(wide.as_ptr());
    }
}

fn main() -> Result<()> {
    // Parse before tracing init so `--version` exits with clean stdout —
    // the updater and scripts parse it (ADR 0030 §1).
    let cli = cli::Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Set the taskbar AUMID before any window exists so the running window
    // merges into the pinned shortcut's button (Windows-only; see above).
    #[cfg(windows)]
    set_app_user_model_id();

    // Remote-role release installs stage their own platform's update in the
    // background (ADR 0030 Phase C3); no-op everywhere else. Must not depend
    // on the backend connection — see selfupdate.rs.
    selfupdate::spawn_startup_selfcheck();

    tracing::info!("sot-frontend starting");
    tracing::info!(
        socket = ?cli.socket,
        tcp = ?cli.tcp,
        token_set = cli.token.is_some(),
        capture = ?cli.capture,
        scale = cli.scale,
        start_mode = %cli.start_mode,
        "cli parsed"
    );

    let event_loop = EventLoop::new()?;
    // Capture mode keeps redrawing until the trigger frame so the transport
    // task has time to deliver hello/tree/preview events; interactive mode
    // sleeps between events.
    event_loop.set_control_flow(if cli.capture.is_some() {
        ControlFlow::Poll
    } else {
        ControlFlow::Wait
    });

    // ADR 0042 L2a: the connection set is every hosts.toml entry with a
    // reachable endpoint (local first, then hosts.toml order), with the CLI
    // --socket/--tcp/--token flags overriding whichever entry is
    // `default_host` — same meaning those flags always had, now scoped to
    // one host instead of "the only host". No hosts.toml (or no matching
    // default entry) falls back to exactly the old single-connection
    // CLI-only behaviour (see `hosts::resolve_connections`).
    let hosts_cfg = hosts::load();
    let cli_override = hosts::CliOverride {
        socket: cli.socket.clone(),
        tcp: cli.tcp.clone(),
        token: cli.token.clone(),
    };
    let connections = hosts::resolve_connections(&hosts_cfg, &cli_override);
    tracing::info!(hosts = ?connections.iter().map(|(h, _)| h.clone()).collect::<Vec<_>>(), "connection set resolved");

    // Channel from every transport task → GPU thread, fanned in. std::sync::mpsc
    // because the GPU thread drains it non-blockingly each redraw; tokio mpsc
    // would require an async drain. One receiver, N cloned senders (one per
    // host's transport task) — tagging happens at each transport's own send,
    // not through a forwarding task.
    let (evt_tx, evt_rx) = mpsc::channel::<(hosts::HostKey, transport::IncomingEvt)>();

    // One outgoing-request channel per host: the GPU thread's `conns` sender
    // half is built here; the receiver half travels with its `TransportConfig`
    // into `App` until `resumed()` spawns that host's transport task.
    let mut conns = Vec::with_capacity(connections.len());
    let mut pending_transports = Vec::with_capacity(connections.len());
    for (host, config) in connections {
        let (req_tx, req_rx) = transport::outgoing_channel();
        conns.push((host.clone(), req_tx));
        pending_transports.push((host, config, req_rx));
    }

    let rt = if !pending_transports.is_empty() {
        Some(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(1)
                .thread_name("sot-transport")
                .build()?,
        )
    } else {
        tracing::info!(
            "no reachable host (no --socket / --tcp / $SOT_SOCKET / $SOT_TCP / hosts.toml entry); running offline against bundled samples"
        );
        None
    };

    let mut app = gpu::App::new(evt_rx, rt, cli, evt_tx, conns, Some(pending_transports));
    event_loop.run_app(&mut app)?;
    Ok(())
}
