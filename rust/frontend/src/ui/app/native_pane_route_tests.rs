//! The pane_timing routes: a stand-in bridge, or two real ssh logins through a test-owned stand-in for the hub's relay unit; and the route preflight.

use super::native_pane_daemon_tests::{reap, Control, Daemon};
use anyhow::{ensure, Context, Result};
use sot_protocol::topology::ssh_bridge::{LinkGate, SshRecipe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(super) const HUB: &str = "t3hub";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RouteKind {
    SshRelay,
    StandIn,
}

impl RouteKind {
    pub(super) fn name(self) -> &'static str {
        match self {
            RouteKind::SshRelay => "ssh-relay",
            RouteKind::StandIn => "stand-in",
        }
    }
}

/// The scheme the stand-in bridge's `--endpoint` takes on this platform.
fn endpoint_arg(endpoint: &Path) -> String {
    format!("{}{}", if cfg!(windows) { "pipe:" } else { "unix:" }, endpoint.display())
}

/// A chosen route: the recipe the window dials, and (relayed) the hub-side acceptor behind it.
pub(super) struct Route {
    pub(super) kind: RouteKind,
    pub(super) host: String,
    program: PathBuf,
    args: Vec<String>,
    #[cfg(unix)]
    relay: Option<Relay>,
}

impl Route {
    pub(super) fn start(kind: RouteKind, sotd: &Path, endpoint: &Path) -> Result<Route> {
        let host = format!("t3relay{}", std::process::id());
        match kind {
            RouteKind::StandIn => Ok(Route {
                kind,
                host: "t3row".into(),
                program: sotd.to_path_buf(),
                args: vec!["stdio-bridge".into(), "--endpoint".into(), endpoint_arg(endpoint)],
                #[cfg(unix)]
                relay: None,
            }),
            #[cfg(unix)]
            RouteKind::SshRelay => unix::start_relayed(&host, sotd, endpoint),
            #[cfg(not(unix))]
            RouteKind::SshRelay => anyhow::bail!("pane-timing ssh-relay route is Unix-only"),
        }
    }

    pub(super) fn recipe(&self) -> SshRecipe {
        recipe_from(&self.host, &self.program, &self.args)
    }

    pub(super) fn program_and_args(&self) -> (&Path, &[String]) {
        (&self.program, &self.args)
    }

    /// Relay connections the acceptor has taken so far; `None` on a route with no relay hop.
    pub(super) fn relay_accepts(&self) -> Option<usize> {
        #[cfg(unix)]
        return self.relay.as_ref().map(|r| r.accepts.load(Ordering::SeqCst));
        #[cfg(not(unix))]
        None
    }

    /// Stops the acceptor, ends every hop-2 child it recorded and removes the hub folder; every problem is reported.
    pub(super) fn teardown(&mut self) -> Result<()> {
        #[cfg(unix)]
        if let Some(relay) = self.relay.take() {
            return relay.stop();
        }
        Ok(())
    }
}

pub(super) fn recipe_from(host: &str, program: &Path, args: &[String]) -> SshRecipe {
    SshRecipe::new(HUB, Some(host)).expect("plain host names").with_test_command(program, args.iter().cloned())
}

/// The route's evidence before any timing: a control child reaches the private daemon, the relay hop (if any) carried
/// it, and a real attach through the same recipe checkpoints Ready row 0 with its sentinel.
pub(super) fn preflight(route: &Route, daemon: &Daemon) -> Result<()> {
    let name = route.kind.name();
    let child = LinkGate::default().spawn_sync(&route.recipe()).map_err(|e| anyhow::anyhow!("{name} route could not start: {e}"))?;
    let mut control = Control::from_child(child, Duration::from_secs(30))?;
    let session = control.hello("t3-preflight")?;
    ensure!(session == daemon.session_id, "{name} route reached another daemon");
    if route.kind == RouteKind::SshRelay {
        ensure!(route.relay_accepts().unwrap_or(0) >= 1, "relayed route used no relay hop");
    }
    control.close();
    let started = Instant::now();
    let row = &daemon.ready_rows()[0];
    let screen = attach_and_read(route, &row.session)?;
    ensure!(screen.contains(&row.nonce), "{name} preflight attach did not show the row's sentinel");
    println!("pane-timing preflight route={name} session=matched checkpoint_ms={} sentinel=true", started.elapsed().as_millis());
    Ok(())
}

/// One headless attach over the route's recipe: waits for the checkpoint and returns the screen's text.
fn attach_and_read(route: &Route, session: &str) -> Result<String> {
    use sot_log::attach_client::client::FeAttachClient;
    use sot_protocol::topology::lane_client::{DaemonLaneEndpoint, LaneDial};
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Ssh(route.recipe(), LinkGate::default()), None);
    let mut client = FeAttachClient::attach(endpoint, session.to_string(), 80, 24, "t3-preflight".into(), "t3-preflight".into(), None, Box::new(|| {}))
        .map_err(|e| anyhow::anyhow!("preflight attach: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        ensure!(!client.is_dead(), "preflight client died: {}", client.status_line());
        ensure!(Instant::now() < deadline, "preflight attach never checkpointed");
        std::thread::sleep(Duration::from_millis(5));
    }
    client.pump();
    let text = client.screen().contents();
    client.shutdown(Duration::from_secs(5));
    Ok(text)
}

#[cfg(unix)]
struct Relay {
    accepts: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    hop2: Arc<Mutex<Vec<std::process::Child>>>,
    hub_dir: PathBuf,
}

#[cfg(unix)]
impl Relay {
    fn stop(mut self) -> Result<()> {
        let mut problems = Vec::new();
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                problems.push("relay acceptor panicked".to_string());
            }
        }
        let children = std::mem::take(&mut *self.hop2.lock().unwrap());
        for mut child in children {
            if !reap(&mut child, Duration::from_secs(10)) {
                problems.push("a relay hop-2 child was not reaped in its bound".to_string());
            }
        }
        if let Err(e) = std::fs::remove_dir_all(&self.hub_dir) {
            problems.push(format!("remove {}: {e}", self.hub_dir.display()));
        }
        ensure!(problems.is_empty(), "{}", problems.join("; "));
        Ok(())
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use sot_protocol::topology::ssh_bridge::SSH_OPTS;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Stdio};

    pub(super) fn start_relayed(host: &str, sotd: &Path, endpoint: &Path) -> Result<Route> {
        let hub = std::path::PathBuf::from(format!("/tmp/sot-t3hub-{}-{}", std::process::id(), nanos()));
        std::fs::create_dir(&hub)?;
        std::fs::set_permissions(&hub, std::fs::Permissions::from_mode(0o700))?;
        ensure!(sot_log::host::state_dir::is_private_dir(&hub), "the hub runtime dir is not private");
        println!("pane-timing roots hub={}", hub.display());
        let socket = hub.join(format!("sot-host-{host}.sock"));
        let listener = UnixListener::bind(&socket).context("bind the relay socket")?;
        listener.set_nonblocking(true)?;
        let accepts = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let hop2 = Arc::new(Mutex::new(Vec::new()));
        let remote2 = format!("test -S {ep} && {sotd} stdio-bridge --endpoint unix:{ep}", ep = endpoint.display(), sotd = sotd.display());
        let thread = {
            let (accepts, stop, hop2) = (accepts.clone(), stop.clone(), hop2.clone());
            std::thread::spawn(move || accept_loop(&listener, &remote2, &accepts, &stop, &hop2))
        };
        let remote1 = format!(
            "test -S {socket} && env -u SOT_RUNTIME_DIR XDG_RUNTIME_DIR={hub} {sotd} stdio-bridge --host {host}",
            socket = socket.display(),
            hub = hub.display(),
            sotd = sotd.display(),
        );
        let mut args: Vec<String> = SSH_OPTS.iter().map(|s| s.to_string()).collect();
        args.extend(["localhost".to_string(), remote1]);
        Ok(Route {
            kind: RouteKind::SshRelay,
            host: host.to_string(),
            program: "ssh".into(),
            args,
            relay: Some(Relay { accepts, stop, thread: Some(thread), hop2, hub_dir: hub }),
        })
    }

    fn nanos() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
    }

    /// The stand-in for the hub's per-connection relay unit: each accepted connection becomes stdin and stdout of hop 2.
    fn accept_loop(listener: &UnixListener, remote: &str, accepts: &AtomicUsize, stop: &AtomicBool, hop2: &Mutex<Vec<std::process::Child>>) {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    accepts.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.set_nonblocking(false);
                    let spawned = (|| -> std::io::Result<std::process::Child> {
                        let input = OwnedFd::from(stream.try_clone()?);
                        let output = OwnedFd::from(stream);
                        Command::new("ssh")
                            .args(SSH_OPTS)
                            .arg("localhost")
                            .arg(remote)
                            .stdin(Stdio::from(input))
                            .stdout(Stdio::from(output))
                            .stderr(Stdio::null())
                            .spawn()
                    })();
                    match spawned {
                        Ok(child) => hop2.lock().unwrap().push(child),
                        Err(e) => eprintln!("pane-timing relay hop 2 could not start: {e}"),
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    eprintln!("pane-timing relay accept: {e}");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}
