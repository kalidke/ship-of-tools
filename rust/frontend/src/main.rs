// sot-frontend
//
// Native local window. Owns rendering end-to-end. No terminal protocols.
// See ADR 0003 (rendering surface), ADR 0011 (rendering split — ratatui chrome
// vs. preview-layer), ADR 0012 (frontend stack).
//
// The transport task is spawned on a dedicated tokio runtime alongside winit.
// winit drives the main thread for window/input/redraw; tokio carries the
// Unix-socket protocol traffic to/from the backend (ADR 0010).

mod browser_open;
mod cli;
mod lease;
mod net;
mod pages;
mod relaunch;
mod selfupdate;
mod ui;

use std::sync::mpsc;

use anyhow::Result;
use sot_log::secret::RedactingWriter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
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

/// The window's log: events at the level `filter` passes (`main` gives the `RUST_LOG` level, default `info`), each masked of page secrets
/// (`sot_log::secret`) and written, without colour codes, to a writer from `make`.
fn log_subscriber<W: std::io::Write + 'static>(
    filter: EnvFilter,
    make: impl Fn() -> W + Send + Sync + 'static,
) -> impl tracing::Subscriber + Send + Sync {
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(move || RedactingWriter(make()))
        .finish()
}

fn window_entry<T>(startup: impl FnOnce() -> Result<T>) -> Result<T> {
    #[cfg(windows)]
    if let Err(e) = Ok::<(), std::io::Error>(()) {
        eprintln!("sot-fe: could not harden inherited stdio ({e}); continuing");
    }
    startup()
}

#[cfg(all(test, feature = "test-window-progress"))]
fn main() -> Result<()> {
    window_entry(ui::run_native_window_progress)
}

#[cfg(not(all(test, feature = "test-window-progress")))]
fn main() -> Result<()> {
    window_entry(ordinary_startup)
}

#[cfg(not(all(test, feature = "test-window-progress")))]
fn ordinary_startup() -> Result<()> {
    // Parse before tracing init so `--version` exits with clean stdout —
    // the updater and scripts parse it (ADR 0030 §1).
    let cli = cli::Cli::parse();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    log_subscriber(filter, std::io::stdout).init();

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
    // config file for hosts (see `net/dial.rs`; no hosts.toml, here or
    // anywhere else).
    let mut dials: Vec<(net::dial::HostKey, net::transport::TransportConfig)> = Vec::new();
    for arg in &cli.dial {
        match net::dial::parse_dial_arg(arg) {
            Ok(entry) => dials.push(entry),
            Err(e) => tracing::warn!("{e}; skipping"),
        }
    }
    let cli_override = net::dial::CliOverride {
        socket: cli.socket.clone(),
        token: cli.token.clone(),
    };
    let connections = net::dial::resolve_connections(&dials, &cli_override);
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
    let (evt_tx, evt_rx) = mpsc::channel::<(net::dial::HostKey, net::transport::IncomingEvt)>();

    // One outgoing-request channel per host: the GPU thread's `conns` sender
    // half is built here; the receiver half travels with its `TransportConfig`
    // into `App` until `resumed()` spawns that host's transport task.
    let mut conns = Vec::with_capacity(connections.len());
    let mut pending_transports = Vec::with_capacity(connections.len());
    for (host, config) in connections {
        let (req_tx, req_rx) = net::transport::outgoing_channel();
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

    let mut app = ui::App::new(evt_rx, rt, cli, evt_tx, conns, Some(pending_transports), leases);
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR 0049, User isolation: the window's log never holds a page secret an event carries.
    #[test]
    fn the_window_log_masks_page_secrets() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!("sot-window-log-{}-{nanos}.log", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let subscriber = log_subscriber(EnvFilter::new("trace"), move || file.try_clone().unwrap());
        let token = "0123456789abcdef0123456789abcdef";
        let _log = sot_log::test_log::install(subscriber);
        tracing::error!(%token, "open http://127.0.0.1:1/x?secret=Ab12Cd34");
        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(written.contains("<redacted>"), "the event did not reach the file: {written}");
        assert!(!written.contains(token), "the token reached the file: {written}");
        assert!(!written.contains("Ab12Cd34"), "the secret reached the file: {written}");
        // No colour codes: on Windows this output is a file, and they would split a field name from its `=`.
        assert!(!written.contains('\u{1b}'), "the file carries ANSI escapes: {written:?}");
    }

    #[cfg(windows)]
    fn stdio_flags() -> [u32; 3] {
        use windows_sys::Win32::Foundation::GetHandleInformation;
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|which| {
            let mut flags = 0;
            assert_ne!(
                unsafe { GetHandleInformation(GetStdHandle(which), &mut flags) },
                0
            );
            flags
        })
    }

    #[cfg(windows)]
    fn owned_startup_probe(
        root: &std::path::Path,
    ) -> Result<(std::process::Child, sot_log::test_isolated::Entry)> {
        use std::process::{Command, Stdio};
        let program = root.join("startup-probe.exe");
        sot_log::test_exec::write_executable(&program, std::fs::read(std::env::current_exe()?)?);
        let (recipe, entry) =
            sot_log::test_isolated::test_command("tests::window_startup_owned_probe");
        let mut command = Command::new(program);
        command.args(recipe.get_args());
        for (name, value) in recipe.get_envs() {
            if let Some(value) = value {
                command.env(name, value);
            } else {
                command.env_remove(name);
            }
        }
        let probe = command
            .env("SOT_TEST_WINDOW_STARTUP_PROBE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok((probe, entry))
    }

    #[cfg(windows)]
    #[test]
    fn window_startup_hardens_handles_before_starting_children() {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        use windows_sys::Win32::System::Console::{
            GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        if !sot_log::test_isolated::run_isolated(
            "tests::window_startup_hardens_handles_before_starting_children",
        ) {
            return;
        }
        let _home = crate::net::state::test_env::set_test_env();
        let root = std::path::PathBuf::from(
            std::env::var_os("XDG_STATE_HOME").expect("fixture state root"),
        );
        let paths = ["input", "output", "error"].map(|name| root.join(name));
        let files = paths.each_ref().map(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap()
        });
        let which = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
        let saved = which.map(|handle| unsafe { GetStdHandle(handle) });
        struct Restore([windows_sys::Win32::Foundation::HANDLE; 3]);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (which, handle) in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
                    .into_iter()
                    .zip(self.0)
                {
                    unsafe {
                        SetStdHandle(which, handle);
                    }
                }
            }
        }
        let restore = Restore(saved);
        for (which, file) in which.into_iter().zip(&files) {
            assert_ne!(
                unsafe {
                    SetHandleInformation(
                        file.as_raw_handle() as _,
                        HANDLE_FLAG_INHERIT,
                        HANDLE_FLAG_INHERIT,
                    )
                },
                0
            );
            assert_ne!(unsafe { SetStdHandle(which, file.as_raw_handle() as _) }, 0);
        }
        assert!(stdio_flags()
            .iter()
            .all(|flags| flags & HANDLE_FLAG_INHERIT != 0));
        let (flags, mut probe, entry) = window_entry(|| {
            let flags = stdio_flags();
            let (probe, entry) = owned_startup_probe(&root)?;
            Ok((flags, probe, entry))
        })
        .unwrap();
        drop(restore);
        drop(files);
        let exclusive = paths.each_ref().map(|path| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(path)
        });
        probe.stdin.take();
        let pid = probe.id();
        let (status, stdout, stderr) =
            sot_log::test_isolated::drain(probe).wait_within(std::time::Duration::from_secs(5));
        assert!(status.success(), "owned startup probe failed: {stderr}");
        entry.assert_once(pid);
        assert!(
            stdout.contains("startup-probe entered"),
            "the owned startup probe never entered"
        );
        assert!(
            flags.iter().all(|flags| flags & HANDLE_FLAG_INHERIT == 0),
            "window_entry left inherited standard handles before its startup continuation"
        );
        assert!(
            exclusive.iter().all(Result::is_ok),
            "the first owned child retained a fixture standard file"
        );
        println!("window-startup flags_cleared=3 child_retained_files=0 entered_bodies=1 completed_bodies=1");
    }

    #[cfg(windows)]
    #[test]
    fn window_startup_owned_probe() {
        if std::env::var("SOT_TEST_WINDOW_STARTUP_PROBE").as_deref() != Ok("1") {
            return;
        }
        sot_log::test_isolated::enter("tests::window_startup_owned_probe");
        use std::io::{Read, Write};
        println!("startup-probe entered");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
    }
}
