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
mod help;
mod dial;
mod keybindings;
mod layout;
mod lease;
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
        token_set = cli.token.is_some(),
        capture = ?cli.capture,
        scale = cli.scale,
        start_mode = ?cli.start_mode,
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

    // Topology plan (lane D): the connection set comes ONLY from repeated
    // `--dial <host>=<endpoint>` flags — the launcher's own rendering of
    // `sotd topology plan --self <host>` — plus the CLI --socket/--token
    // flags, which override the implicit "local" connection. Those CLI
    // flags are the ad hoc/manual path (dev, tests, a box with no launcher
    // or topology plan at all); the launcher itself never needs them,
    // since the plan hands this box's own endpoint over as an ordinary
    // `--dial` entry. There is no `--tcp` twin (C3): a remote box in the
    // ad hoc path is `--dial local=ssh:<target>`. The frontend reads no
    // config file for hosts (see `dial.rs`; no hosts.toml, here or
    // anywhere else).
    let mut dials: Vec<(dial::HostKey, transport::TransportConfig)> = Vec::new();
    for arg in &cli.dial {
        match dial::parse_dial_arg(arg) {
            Ok(entry) => dials.push(entry),
            Err(e) => tracing::warn!("{e}; skipping"),
        }
    }
    let cli_override = dial::CliOverride {
        socket: cli.socket.clone(),
        token: cli.token.clone(),
    };
    let connections = dial::resolve_connections(&dials, &cli_override);
    let leases = lease::Leases::new(
        lease::lease_exempt(cli.ephemeral, cli.capture.is_some(), cli.no_lease),
        lease::pipe_hosts(&connections),
    );
    tracing::info!(hosts = ?connections.iter().map(|(h, _)| h.clone()).collect::<Vec<_>>(), "connection set resolved");

    // Channel from every transport task → GPU thread, fanned in. std::sync::mpsc
    // because the GPU thread drains it non-blockingly each redraw; tokio mpsc
    // would require an async drain. One receiver, N cloned senders (one per
    // host's transport task) — tagging happens at each transport's own send,
    // not through a forwarding task.
    let (evt_tx, evt_rx) = mpsc::channel::<(dial::HostKey, transport::IncomingEvt)>();

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
            "no reachable host (no --dial / --socket / $SOT_SOCKET); running offline against bundled samples"
        );
        None
    };

    let mut app = gpu::App::new(evt_rx, rt, cli, evt_tx, conns, Some(pending_transports), leases);
    let ran = event_loop.run_app(&mut app);
    shutdown_transport(app.take_runtime());
    ran?;
    Ok(())
}

/// How long the exit waits for the transport runtime's blocking work (child
/// stdio reads, pipe connects). The runtime's own drop waits without bound,
/// and a window that stays up un-pumped meanwhile is reported as Not
/// Responding; `shutdown_timeout` still drops every task, so the ssh children
/// (`kill_on_drop`) end with it.
const RUNTIME_SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

fn shutdown_transport(rt: Option<tokio::runtime::Runtime>) {
    if let Some(rt) = rt {
        rt.shutdown_timeout(RUNTIME_SHUTDOWN_WAIT);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn transport_shutdown_is_bounded_and_kills_children() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        // Blocking work that never finishes, as a child-stdio read does.
        rt.spawn(async { let _ = tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(60))).await; });
        let (pid_tx, pid_rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let mut child = tokio::process::Command::new("sleep").arg("60").kill_on_drop(true).spawn().unwrap();
            pid_tx.send(child.id().unwrap()).unwrap();
            let _ = child.wait().await;
        });
        let pid = pid_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let t = Instant::now();
        std::thread::spawn(move || {
            shutdown_transport(Some(rt));
            let _ = done_tx.send(());
        });
        done_rx.recv_timeout(Duration::from_secs(2)).expect("the runtime shutdown waited past its bound");
        assert!(t.elapsed() < Duration::from_secs(2));
        let gone = |pid: u32| match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(s) => s.rsplit(')').next().is_some_and(|r| r.trim_start().starts_with('Z')),
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !gone(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(gone(pid), "the kill_on_drop child outlived the runtime");
    }
}
