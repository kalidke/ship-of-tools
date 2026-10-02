//! Frontend half of the daemon TCP proxy (ADR 0035), C3 as amended §3.
//!
//! A REMOTE frontend reaches any backend-served loopback page (Pluto, video,
//! docs + pool, WGLMakie/Bonito) through an ssh child to that page's OWNING
//! daemon — no per-port ssh `-L` forward, no launcher edits when a new
//! backend port appears. A multi-host FE spawns one such child per browser
//! connection (W2, the accepted cost — isolation-plan.md §10), so each
//! listener carries its OWN target recipe (never one baked-in default-host
//! address for every port — that was the cross-host figure defect: a page
//! served by a non-default host's daemon had nowhere to proxy through). The
//! browser still opens a plain `http://127.0.0.1:<port>/…` URL; this module
//! makes that loopback port resolve by binding a local listener that pipes
//! each browser connection to the right daemon, which dials the real service
//! (the daemon half validates the port + does the dialing —
//! `backend/src/proxy.rs`).
//!
//! Ownership split that keeps the "bind before the browser launches" ordering
//! honest without blocking the render thread: the GPU thread binds a
//! `std::net::TcpListener` SYNCHRONOUSLY (a bind is sub-millisecond, no
//! `block_on`, so the port is already listening the instant
//! `open_url_in_browser` runs) and hands the bound listener — tagged with the
//! ssh recipe and token it resolved for that page's host — to the transport
//! runtime here, which owns the async accept loop + the per-connection pipe.

use std::net::TcpListener as StdTcpListener;

use sot_protocol::ssh_bridge::{LinkGate, SpawnError, SshRecipe};
use sot_protocol::{codec, op, Frame, ProxyConnectReq};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedReceiver;

/// Spawn the proxy manager on the transport runtime. It receives bound
/// listeners from the GPU thread (`State::ensure_proxy_for_url`), each
/// tagged with `(recipe, token)` — the exact ssh recipe and token resolved
/// for the page's OWNING host, not a single manager-wide default. Per
/// listener it runs an accept loop that pipes each accepted browser
/// connection to a FRESH ssh child spawned from that listener's own
/// `recipe`; `token` is forwarded in the handshake when that daemon has one
/// configured (Unix-socket transports carry none).
pub fn spawn_proxy_manager(
    rt: &tokio::runtime::Runtime,
    mut listener_rx: UnboundedReceiver<(StdTcpListener, SshRecipe, Option<String>, LinkGate)>,
) {
    rt.spawn(async move {
        while let Some((std_listener, recipe, token, gate)) = listener_rx.recv().await {
            let port = match std_listener.local_addr() {
                Ok(a) => a.port(),
                Err(e) => {
                    tracing::warn!(error = %e, "proxy: listener with no local_addr; dropping");
                    continue;
                }
            };
            // The GPU thread already set it non-blocking; from_std needs that.
            let listener = match tokio::net::TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(port, error = %e, "proxy: from_std failed; dropping listener");
                    continue;
                }
            };
            tokio::spawn(async move {
                tracing::info!(port, %recipe, "proxy: accepting browser connections for backend port");
                loop {
                    match listener.accept().await {
                        Ok((browser, _peer)) => {
                            let recipe = recipe.clone();
                            let token = token.clone();
                            let gate = gate.clone();
                            tokio::spawn(async move {
                                // A dead child here is a blank page with no
                                // other carrier — `debug!` → `warn!` (C3 as
                                // amended §6): a reason at `debug` is
                                // invisible in a normal run.
                                if let Err(e) = pipe_one(browser, &recipe, &gate, port, token.as_deref()).await {
                                    tracing::warn!(port, error = %e, "proxy: connection ended");
                                }
                            });
                        }
                        Err(e) => {
                            // A transient accept error shouldn't kill the
                            // listener; back off a beat and keep serving.
                            tracing::warn!(port, error = %e, "proxy: accept error");
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    }
                }
            });
        }
        tracing::debug!("proxy: listener channel closed; manager exiting");
    });
}

/// Pipe one browser connection through an ssh child spawned from `recipe`
/// for `port` (C3 as amended §3): spawn, do the `proxy.connect` handshake
/// as the child's first bytes — the daemon peeks that op on ANY accepted
/// connection (`server.rs`), so the frames are byte-identical to the old
/// tcp-forwarded leg — then splice bytes both ways until either side closes
/// (carrying a WebSocket upgrade verbatim). While the host's link is down
/// (`gate`) no child is spawned and the browser connection closes at once.
async fn pipe_one(
    browser: tokio::net::TcpStream,
    recipe: &SshRecipe,
    gate: &LinkGate,
    port: u16,
    token: Option<&str>,
) -> anyhow::Result<()> {
    let mut child = match gate.spawn_async(recipe) {
        Ok(child) => child,
        Err(SpawnError::LinkDown) => return Err(SpawnError::LinkDown.into()),
        Err(e) => return Err(anyhow::Error::new(e).context(format!("spawn ssh {recipe}"))),
    };
    let d_wr = child.stdin.take().expect("spawned with a piped stdin");
    let d_rd = child.stdout.take().expect("spawned with a piped stdout");
    let stderr = child.stderr.take().expect("spawned with a piped stderr");
    let last_stderr = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    {
        let last_stderr = std::sync::Arc::clone(&last_stderr);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    if let Ok(mut guard) = last_stderr.lock() {
                        *guard = Some(line);
                    }
                }
            }
        });
    }
    let mut d_wr = d_wr;
    let _ = browser.set_nodelay(true);

    // Handshake read goes through a BufReader; the pipe below copies FROM that
    // same BufReader so any bytes it buffered past the response envelope (the
    // daemon may start streaming upstream data immediately after `{ok}`) are
    // not lost — the throwaway-reader trap.
    let mut d_buf = BufReader::new(d_rd);
    let result = pipe_one_over(&mut d_wr, &mut d_buf, browser, port, token).await;
    // `child` drops here (`kill_on_drop`), ending this connection's ssh
    // login. The child's last stderr line is the diagnosis when it died
    // before or during the splice — beats a generic broken-pipe message.
    if result.is_err() {
        if let Some(line) = sot_protocol::ssh_bridge::last_stderr_after_failure(&last_stderr).await {
            return Err(anyhow::anyhow!(line));
        }
    }
    result
}

async fn pipe_one_over(
    d_wr: &mut tokio::process::ChildStdin,
    d_buf: &mut BufReader<tokio::process::ChildStdout>,
    browser: tokio::net::TcpStream,
    port: u16,
    token: Option<&str>,
) -> anyhow::Result<()> {

    let req = ProxyConnectReq {
        port,
        token: token.map(|s| s.to_string()),
    };
    let frame = Frame::req(1, op::PROXY_CONNECT, serde_json::to_value(&req)?);
    codec::write_frame(d_wr, &frame, None).await?;
    d_wr.flush().await?;

    let (res, _blob) = codec::read_frame(d_buf).await?;
    if res.payload.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let code = res
            .payload
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        anyhow::bail!("daemon refused proxy.connect for port {port}: {code}");
    }

    let (mut b_rd, mut b_wr) = browser.into_split();
    let browser_to_daemon = async {
        let r = tokio::io::copy(&mut b_rd, d_wr).await;
        let _ = d_wr.shutdown().await; // half-close so the daemon sees EOF
        r
    };
    let daemon_to_browser = async {
        let r = tokio::io::copy(d_buf, &mut b_wr).await;
        let _ = b_wr.shutdown().await;
        r
    };
    // Tear down as soon as EITHER direction closes. A `join!` would wait for
    // BOTH, leaking the task + both sockets if one side half-opens (holds its
    // write half open after the other EOFs) — unbounded growth under repeated
    // connections (codex). `select!` drops the losing copy; the owned split
    // halves drop at return, closing both sockets.
    tokio::select! {
        r = browser_to_daemon => tracing::debug!(port, ?r, "proxy: browser→daemon closed first"),
        r = daemon_to_browser => tracing::debug!(port, ?r, "proxy: daemon→browser closed first"),
    }
    Ok(())
}

/// Parse the loopback port out of a `http(s)://127.0.0.1:<port>/…` URL — the
/// shape every backend-served page carries (video/docs/WGL). Returns `None`
/// for any non-loopback host or portless URL, so only the daemon's own
/// loopback pages arm a proxy listener.
pub fn proxy_port_from_url(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    // authority is up to the first '/', '?' or '#'
    let authority = rest.split(['/', '?', '#']).next()?;
    let (host, port) = authority.rsplit_once(':')?;
    // Loopback only — never arm a listener for an external host.
    if host != "127.0.0.1" && host != "localhost" {
        return None;
    }
    port.parse::<u16>().ok()
}

#[cfg(test)]
mod tests {
    /// ADR 0045 decision 4: with the host's link down a browser connection
    /// closes at once with `LinkDown` and no ssh child is spawned.
    #[tokio::test]
    async fn a_down_gate_closes_every_browser_connection_without_a_spawn() {
        use tokio::io::AsyncReadExt;
        let gate = sot_protocol::ssh_bridge::LinkGate::default();
        gate.set_up(false);
        let recipe = sot_protocol::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        for _ in 0..5 {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (browser, _) = listener.accept().await.unwrap();
            let t0 = std::time::Instant::now();
            let err = super::pipe_one(browser, &recipe, &gate, addr.port(), None).await.unwrap_err();
            assert!(
                matches!(err.downcast_ref::<sot_protocol::ssh_bridge::SpawnError>(), Some(sot_protocol::ssh_bridge::SpawnError::LinkDown)),
                "got {err:#}"
            );
            let mut buf = [0u8; 1];
            let n = tokio::time::timeout(std::time::Duration::from_millis(200), client.read(&mut buf))
                .await
                .expect("the browser connection must close at once")
                .unwrap_or(0);
            assert_eq!(n, 0);
            assert!(t0.elapsed() < std::time::Duration::from_millis(200));
        }
    }

    use super::proxy_port_from_url;

    #[test]
    fn parses_loopback_ports_only() {
        assert_eq!(proxy_port_from_url("http://127.0.0.1:1241/"), Some(1241));
        assert_eq!(
            proxy_port_from_url("http://127.0.0.1:1237/foo/bar?secret=abc"),
            Some(1237)
        );
        assert_eq!(proxy_port_from_url("https://localhost:1235/tok"), Some(1235));
        // Non-loopback host → None (never proxy an external address).
        assert_eq!(proxy_port_from_url("http://example.com:80/"), None);
        assert_eq!(proxy_port_from_url("http://10.0.0.5:1234/"), None);
        // No port, or non-http scheme, or garbage → None.
        assert_eq!(proxy_port_from_url("http://127.0.0.1/"), None);
        assert_eq!(proxy_port_from_url("file:///tmp/x"), None);
        assert_eq!(proxy_port_from_url("127.0.0.1:1241"), None);
        assert_eq!(proxy_port_from_url("http://127.0.0.1:notaport/"), None);
    }
}
