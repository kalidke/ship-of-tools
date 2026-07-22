//! Frontend half of the daemon TCP proxy (ADR 0035).
//!
//! A REMOTE frontend reaches any backend-served loopback page (Pluto, video,
//! docs + pool, WGLMakie/Bonito) through the ONE control tunnel it already
//! holds — no per-port ssh `-L` forward, no launcher edits when a new backend
//! port appears. The browser still opens a plain `http://127.0.0.1:<port>/…`
//! URL; this module makes that loopback port resolve by binding a local
//! listener that pipes each browser connection to the daemon, which dials the
//! real service (the daemon half validates the port + does the dialing —
//! `backend/src/proxy.rs`).
//!
//! Ownership split that keeps the "bind before the browser launches" ordering
//! honest without blocking the render thread: the GPU thread binds a
//! `std::net::TcpListener` SYNCHRONOUSLY (a bind is sub-millisecond, no
//! `block_on`, so the port is already listening the instant
//! `open_url_in_browser` runs) and hands the bound listener to the transport
//! runtime here, which owns the async accept loop + the per-connection pipe.

use std::net::TcpListener as StdTcpListener;

use sot_protocol::{codec, op, Frame, ProxyConnectReq};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;

/// Spawn the proxy manager on the transport runtime. It receives bound
/// listeners from the GPU thread (`ensure_proxy_listener`) and, per listener,
/// runs an accept loop that pipes each accepted browser connection to the
/// daemon over a FRESH connection to `daemon_tcp` (the same `127.0.0.1:<fwd>`
/// address the control transport dials). `token` is forwarded in the handshake
/// when the daemon has one configured (Unix-socket transports carry none).
pub fn spawn_proxy_manager(
    rt: &tokio::runtime::Runtime,
    daemon_tcp: String,
    token: Option<String>,
    mut listener_rx: UnboundedReceiver<StdTcpListener>,
) {
    rt.spawn(async move {
        while let Some(std_listener) = listener_rx.recv().await {
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
            let daemon_tcp = daemon_tcp.clone();
            let token = token.clone();
            tokio::spawn(async move {
                tracing::info!(port, "proxy: accepting browser connections for backend port");
                loop {
                    match listener.accept().await {
                        Ok((browser, _peer)) => {
                            let daemon_tcp = daemon_tcp.clone();
                            let token = token.clone();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    pipe_one(browser, &daemon_tcp, port, token.as_deref()).await
                                {
                                    tracing::debug!(port, error = %e, "proxy: connection ended");
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

/// Pipe one browser connection to `daemon_tcp` for `port`: open a fresh daemon
/// connection, do the `proxy.connect` handshake, then splice bytes both ways
/// until either side closes (carrying a WebSocket upgrade verbatim).
async fn pipe_one(
    browser: TcpStream,
    daemon_tcp: &str,
    port: u16,
    token: Option<&str>,
) -> anyhow::Result<()> {
    let daemon = TcpStream::connect(daemon_tcp).await?;
    let _ = daemon.set_nodelay(true);
    let _ = browser.set_nodelay(true);

    let (d_rd, mut d_wr) = daemon.into_split();
    // Handshake read goes through a BufReader; the pipe below copies FROM that
    // same BufReader so any bytes it buffered past the response envelope (the
    // daemon may start streaming upstream data immediately after `{ok}`) are
    // not lost — the throwaway-reader trap.
    let mut d_buf = BufReader::new(d_rd);

    let req = ProxyConnectReq {
        port,
        token: token.map(|s| s.to_string()),
    };
    let frame = Frame::req(1, op::PROXY_CONNECT, serde_json::to_value(&req)?);
    codec::write_frame(&mut d_wr, &frame, None).await?;
    d_wr.flush().await?;

    let (res, _blob) = codec::read_frame(&mut d_buf).await?;
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
        let r = tokio::io::copy(&mut b_rd, &mut d_wr).await;
        let _ = d_wr.shutdown().await; // half-close so the daemon sees EOF
        r
    };
    let daemon_to_browser = async {
        let r = tokio::io::copy(&mut d_buf, &mut b_wr).await;
        let _ = b_wr.shutdown().await;
        r
    };
    // Either direction closing tears the pipe down; a one-way error at close
    // is expected and not surfaced loudly.
    let (a, b) = tokio::join!(browser_to_daemon, daemon_to_browser);
    tracing::debug!(port, ?a, ?b, "proxy pipe closed");
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
