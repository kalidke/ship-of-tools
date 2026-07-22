//! Daemon TCP proxy (ADR 0035).
//!
//! A remote frontend can reach ANY backend-served loopback HTTP/WebSocket
//! port through the ONE control tunnel it already holds (`-L 18743` → the
//! daemon Unix socket), instead of a static per-port ssh `-L` forward per
//! service (Pluto 1234, video 1235, docs 1236, docs pool 1237-1240,
//! WGLMakie/Bonito 1241). This retires the launcher `-L` sprawl and the
//! recurring "box's launcher predates a port → dead page until relaunch"
//! failure class.
//!
//! A proxy connection is DEDICATED: the client opens a fresh daemon-socket
//! connection and sends `proxy.connect { port }` as its FIRST frame. If the
//! port is allowed and dials, the daemon answers `{ok:true}` and then pipes
//! every subsequent byte verbatim in both directions (`copy_bidirectional`)
//! until either side closes — which is exactly what carries the WebSocket
//! upgrade unmodified. It never joins the multiplexed control loop, so it
//! can't head-of-line-block pty/repl traffic and needs no flow control of
//! its own (native TCP backpressure).

use std::collections::BTreeSet;

use anyhow::Result;
use sot_protocol::{codec, op, Frame, ProxyConnectReq};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Pluto's server port. Hardcoded in the Julia sidecar (`julia/pluto/start.jl`
/// `const PORT = 1234`); the Rust side never held it as a variable, so the
/// allowlist names it explicitly.
const PLUTO_PORT: u16 = 1234;

/// WGLMakie/Bonito default port (ADR 0032). This server lives in the user's
/// REPL child (`julia/repl/src/ShipToolsRepl.jl`, `SOT_WGL_PORT`), NOT in the
/// daemon — so the daemon reads the same env here purely to allow-list it
/// (the "daemon doesn't know 1241" gap called out in ADR 0035 §Context).
const WGL_PORT_DEFAULT: u16 = 1241;

/// The set of loopback ports `proxy.connect` will dial — the daemon's own
/// served HTTP surface. Computed per request so an env override or a
/// runtime-assigned pool port is honored without a restart.
///
/// - Pluto (fixed 1234), video (`SOT_VIDEO_PORT`), docs shared
///   (`SOT_DOCS_PORT`),
/// - the docs pool's currently-ASSIGNED ports (`pool_assigned_ports()`, NOT
///   the full configured range) — so a pool port whose boot bind failed
///   (another process owns it) is never authorized, closing the "proxy dials
///   someone else's loopback service" gap for the dynamic pool (codex),
/// - `SOT_WGL_PORT` (default 1241) — read daemon-side only to allow-list it,
/// - `SOT_PROXY_EXTRA_PORTS` (comma-separated) — escape hatch for a future
///   REPL-served dashboard so a new port needs no daemon release.
///
/// Residual (accepted): the FIXED ports (pluto/video/docs-shared/wgl) are
/// listed as configured, not verified-bound — if one's boot bind failed AND
/// another same-user process holds it, a remote FE (already the authenticated
/// user over the ssh tunnel) could reach that same-user loopback service. It's
/// a same-user-loopback exposure, not a privilege boundary; tracking
/// verified-bound fixed ports is a follow-up.
pub fn allowed_proxy_ports() -> BTreeSet<u16> {
    let mut ports = BTreeSet::new();
    ports.insert(PLUTO_PORT);
    ports.insert(crate::http_serve::video_port());
    ports.insert(crate::site_serve::site_port());
    ports.extend(crate::site_serve::pool_assigned_ports());
    ports.insert(wgl_port());
    for tok in std::env::var("SOT_PROXY_EXTRA_PORTS")
        .unwrap_or_default()
        .split(',')
    {
        if let Ok(p) = tok.trim().parse::<u16>() {
            ports.insert(p);
        }
        // Garbage entries (empty, non-numeric, out-of-range) are ignored —
        // a fat-fingered env var must never widen the allowlist to 0/all.
    }
    ports
}

/// `SOT_WGL_PORT` daemon-side, default 1241. Mirrors the REPL-side default
/// (config duplication accepted per ADR 0035 §3 — both sides default the
/// same; an override must be set in both environments).
fn wgl_port() -> u16 {
    std::env::var("SOT_WGL_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(WGL_PORT_DEFAULT)
}

/// Handle a connection whose first frame was `proxy.connect` (ADR 0035).
/// `rx` is the buffered reader that already consumed that first frame (any
/// bytes it buffered past the envelope are preserved — `copy` drains the
/// BufReader before touching the socket); `tx` is the write half; `frame` is
/// the parsed handshake frame; `expected_token` is the daemon's configured
/// token (if any). Returns when the pipe closes; errors are logged by the
/// caller.
pub async fn handle_proxy_connect<R, W>(
    mut rx: R,
    mut tx: W,
    frame: Frame,
    expected_token: Option<&str>,
) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let req: ProxyConnectReq = match serde_json::from_value(frame.payload) {
        Ok(r) => r,
        Err(e) => {
            return reject(&mut tx, frame.id, "bad_request", &format!("{e}")).await;
        }
    };

    // Auth mirrors the op gate: honored only when the daemon has a token
    // configured. On the normal local Unix-socket transport there is no
    // token — filesystem permissions on the socket are the trust boundary
    // (as for every op; ADR 0035 §3).
    if let Some(expected) = expected_token {
        let presented = req.token.unwrap_or_default();
        if !crate::handlers::constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            tracing::warn!(port = req.port, "proxy.connect rejected: bad token");
            return reject(&mut tx, frame.id, "unauthenticated", "bad or missing token").await;
        }
    }

    // Loopback-only + served-port allowlist. The frame carries no host by
    // design; we dial 127.0.0.1 exclusively, so a compromised/hostile FE
    // can only reach the daemon's OWN loopback services, never arbitrary
    // internal hosts.
    if !allowed_proxy_ports().contains(&req.port) {
        tracing::warn!(port = req.port, "proxy.connect rejected: port not in allowlist");
        return reject(
            &mut tx,
            frame.id,
            "bad_port",
            &format!("port {} is not a proxyable backend port", req.port),
        )
        .await;
    }

    // Dial the backend service. A short connect timeout keeps a wedged
    // target from parking the proxy task forever.
    let dial = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(("127.0.0.1", req.port)),
    )
    .await;
    let mut upstream = match dial {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(port = req.port, error = %e, "proxy.connect dial failed");
            return reject(&mut tx, frame.id, "dial_failed", &format!("{e}")).await;
        }
        Err(_) => {
            tracing::warn!(port = req.port, "proxy.connect dial timed out");
            return reject(&mut tx, frame.id, "dial_failed", "connect timed out").await;
        }
    };
    let _ = upstream.set_nodelay(true);

    // Handshake OK → hand the connection over to a raw bidirectional pipe.
    let res = Frame::res(
        frame.id,
        op::PROXY_CONNECT,
        serde_json::json!({ "ok": true }),
    );
    codec::write_frame(&mut tx, &res, None).await?;
    tx.flush().await?;
    tracing::info!(port = req.port, "proxy.connect established — piping");

    // Manual bidirectional copy: the client side is SPLIT (buffered reader +
    // write half are separate types), so `tokio::io::copy_bidirectional`
    // (which wants one duplex per side) doesn't fit — join two one-way
    // copies instead. Copying FROM the BufReader drains its internal buffer
    // first, so any bytes it read past the handshake envelope are not lost.
    let (mut up_rx, mut up_tx) = upstream.split();
    let client_to_up = async {
        let r = tokio::io::copy(&mut rx, &mut up_tx).await;
        let _ = up_tx.shutdown().await; // half-close so upstream sees EOF
        r
    };
    let up_to_client = async {
        let r = tokio::io::copy(&mut up_rx, &mut tx).await;
        let _ = tx.shutdown().await;
        r
    };
    // Tear down as soon as EITHER direction closes. A `join!` would wait for
    // BOTH, so a half-open peer (upstream EOFs after responding while the
    // client keeps its write half open, or vice versa) would block the other
    // copy forever and leak the task + both sockets — unbounded growth under
    // repeated half-open connections (codex). `select!` drops the losing copy;
    // the stream halves then drop at return, closing both sockets so the
    // stalled peer sees a reset.
    tokio::select! {
        r = client_to_up => tracing::debug!(port = req.port, ?r, "proxy: client→upstream closed first"),
        r = up_to_client => tracing::debug!(port = req.port, ?r, "proxy: upstream→client closed first"),
    }
    Ok(())
}

/// Write a `proxy.connect` rejection frame (standard error payload) and
/// return; the caller closes the connection.
async fn reject<W>(tx: &mut W, id: u64, code: &str, msg: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::json!({ "error": msg, "code": code });
    let f = Frame::res(id, op::PROXY_CONNECT, payload);
    codec::write_frame(tx, &f, None).await?;
    tx.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env is process-global; serialize the env-mutating tests so they don't
    // race each other (other crates' tests run in separate processes).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn allowlist_has_the_known_served_ports_by_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("SOT_PROXY_EXTRA_PORTS");
        std::env::remove_var("SOT_WGL_PORT");
        std::env::remove_var("SOT_VIDEO_PORT");
        std::env::remove_var("SOT_DOCS_PORT");
        let ports = allowed_proxy_ports();
        assert!(ports.contains(&1234), "pluto");
        assert!(ports.contains(&1235), "video default");
        assert!(ports.contains(&1236), "docs default");
        assert!(ports.contains(&1241), "wgl default");
        // Pool ports are allow-listed only when ASSIGNED (bound+served); with
        // an empty pool here, the configured range is NOT blanket-allowed —
        // the tightening that stops the proxy dialing a port the daemon lost
        // the bind on (codex).
        assert!(!ports.contains(&1237), "unassigned pool port not allowlisted");
        assert!(!ports.contains(&1240), "unassigned pool port not allowlisted");
        // A random unrelated port is NOT proxyable.
        assert!(!ports.contains(&8080));
        assert!(!ports.contains(&22));
    }

    #[test]
    fn extra_ports_env_widens_allowlist_and_ignores_garbage() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SOT_PROXY_EXTRA_PORTS", "9000, 9001 ,,notaport,70000,9002");
        let ports = allowed_proxy_ports();
        assert!(ports.contains(&9000));
        assert!(ports.contains(&9001));
        assert!(ports.contains(&9002));
        // "notaport" and "70000" (> u16::MAX) are ignored, not fatal.
        assert!(!ports.contains(&8080));
        std::env::remove_var("SOT_PROXY_EXTRA_PORTS");
    }

    #[test]
    fn wgl_port_env_override_is_honored() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SOT_WGL_PORT", "1250");
        let ports = allowed_proxy_ports();
        assert!(ports.contains(&1250), "overridden wgl port allowed");
        std::env::remove_var("SOT_WGL_PORT");
        // Back to default when unset.
        assert!(allowed_proxy_ports().contains(&1241));
    }

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A localhost echo server standing in for a backend HTTP/WS service.
    /// Returns its port; echoes every connection's bytes until EOF.
    async fn spawn_echo() -> u16 {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        port
    }

    fn connect_frame(port: u16) -> Frame {
        Frame::req(
            1,
            op::PROXY_CONNECT,
            serde_json::json!({ "port": port }),
        )
    }

    /// Read the single `proxy.connect` response frame off the client side.
    async fn read_res<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> serde_json::Value {
        let mut buf = codec::buffered(r);
        let (f, _) = codec::read_frame(&mut buf).await.unwrap();
        f.payload
    }

    #[tokio::test]
    async fn allowed_port_pipes_both_ways_including_after_upgrade() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let echo = spawn_echo().await;
        std::env::set_var("SOT_PROXY_EXTRA_PORTS", echo.to_string());

        let (client, daemon) = tokio::io::duplex(64 * 1024);
        let (dr, dw) = tokio::io::split(daemon);
        let (mut cr, mut cw) = tokio::io::split(client);

        let daemon_fut = handle_proxy_connect(codec::buffered(dr), dw, connect_frame(echo), None);
        let client_fut = async {
            // ONE buffered reader across the whole client side: reading the
            // res frame may buffer piped bytes past its envelope, so the
            // subsequent payload reads MUST come from the same BufReader or
            // those bytes are lost (the throwaway-reader trap).
            let mut cbuf = codec::buffered(&mut cr);
            let (res, _) = codec::read_frame(&mut cbuf).await.unwrap();
            assert_eq!(res.payload.get("ok").and_then(|v| v.as_bool()), Some(true));
            // Bytes shaped like an HTTP request that then "upgrades" — the
            // pipe carries the whole stream verbatim (the WS-upgrade case).
            cw.write_all(b"GET /ws HTTP/1.1\r\nUpgrade: websocket\r\n\r\n")
                .await
                .unwrap();
            cw.write_all(b"\x81\x05hello").await.unwrap(); // a fake WS frame
            let mut got = vec![0u8; 40]; // the HTTP request bytes above
            cbuf.read_exact(&mut got).await.unwrap();
            assert_eq!(&got, b"GET /ws HTTP/1.1\r\nUpgrade: websocket\r\n\r\n");
            let mut got2 = vec![0u8; 7];
            cbuf.read_exact(&mut got2).await.unwrap();
            assert_eq!(&got2, b"\x81\x05hello");
            // Real half-close: shutdown() the write half so the daemon's read
            // side sees EOF (a plain `drop(cw)` does NOT — the split read half
            // `cr`/`cbuf` still holds the DuplexStream open, so the daemon's
            // client→upstream copy never completes and the pipe never tears
            // down; a real socket close EOFs, which this models).
            cw.shutdown().await.unwrap();
        };
        let (dres, _) = tokio::join!(daemon_fut, client_fut);
        dres.unwrap();

        std::env::remove_var("SOT_PROXY_EXTRA_PORTS");
    }

    #[tokio::test]
    async fn disallowed_port_errors_without_piping() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("SOT_PROXY_EXTRA_PORTS");
        let (client, daemon) = tokio::io::duplex(4096);
        let (dr, dw) = tokio::io::split(daemon);
        let (mut cr, _cw) = tokio::io::split(client);
        // 65000 is not a served backend port.
        let daemon_fut = handle_proxy_connect(codec::buffered(dr), dw, connect_frame(65000), None);
        let client_fut = async {
            let res = read_res(&mut cr).await;
            assert_eq!(res.get("code").and_then(|v| v.as_str()), Some("bad_port"));
        };
        let (dres, _) = tokio::join!(daemon_fut, client_fut);
        dres.unwrap();
    }

    #[tokio::test]
    async fn allowed_but_dead_port_reports_dial_failed() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Bind then drop to obtain a port with (almost certainly) no listener.
        let dead = {
            let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            l.local_addr().unwrap().port()
        };
        std::env::set_var("SOT_PROXY_EXTRA_PORTS", dead.to_string());
        let (client, daemon) = tokio::io::duplex(4096);
        let (dr, dw) = tokio::io::split(daemon);
        let (mut cr, _cw) = tokio::io::split(client);
        let daemon_fut = handle_proxy_connect(codec::buffered(dr), dw, connect_frame(dead), None);
        let client_fut = async {
            let res = read_res(&mut cr).await;
            assert_eq!(res.get("code").and_then(|v| v.as_str()), Some("dial_failed"));
        };
        let (dres, _) = tokio::join!(daemon_fut, client_fut);
        dres.unwrap();
        std::env::remove_var("SOT_PROXY_EXTRA_PORTS");
    }
}
