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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::RwLock;

use anyhow::Result;
use sot_protocol::{codec, op, Frame, ProxyConnectReq};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Loopback ports announced by REPL children via their `browser` frames
/// (ADR 0032 `BrowserView` — `wglshow` and user-served dashboards), keyed by
/// workspace so a dead child's grants die with it. This closes the last
/// configured-not-verified allowlist entry: the WGL/Bonito server binds
/// lazily inside the user's REPL child where the daemon can't observe the
/// bind, but the `BrowserView` URL it produces flows THROUGH the daemon as a
/// repl frame on its way to the FE — the supervisor records the port off
/// that frame (`repl.rs`), so the allowlist learns the ACTUAL bound port
/// (ephemeral-fallback aware) with no protocol change and no trust in a
/// static port number.
///
/// Trust: the REPL child is the user's own arbitrary code, and the proxy
/// client is the same authenticated user — user code authorizing a loopback
/// port for the user's own browser adds no privilege over what either side
/// can already do (same standing as `SOT_PROXY_EXTRA_PORTS`).
static BROWSER_PORTS: RwLock<BTreeMap<String, BTreeSet<u16>>> = RwLock::new(BTreeMap::new());

/// Per-workspace cap on announced ports — a backstop against a runaway loop
/// serving in a hot loop, not a real limit (a workspace realistically holds
/// one or two live served pages).
const BROWSER_PORTS_PER_WS: usize = 16;

/// Record a `browser`-frame loopback port for `workspace`. Called by the
/// REPL supervisor when a BrowserView frame passes through it.
pub fn record_browser_port(workspace: &str, port: u16) {
    let mut m = BROWSER_PORTS.write().unwrap_or_else(|p| p.into_inner());
    let set = m.entry(workspace.to_string()).or_default();
    if set.insert(port) {
        while set.len() > BROWSER_PORTS_PER_WS {
            // BTreeSet has no insertion order; evicting the smallest is an
            // arbitrary-but-deterministic backstop.
            let evict = *set.iter().next().expect("non-empty set");
            set.remove(&evict);
        }
        tracing::info!(workspace, port, "proxy: browser-served port recorded (allowlisted)");
    }
}

/// Drop every announced port for `workspace`. Called when its REPL child
/// dies (gen-guarded) and when a respawn begins — a freed port could be
/// re-bound by any other process, so a dead child's grant must not outlive it.
pub fn revoke_browser_ports(workspace: &str) {
    let mut m = BROWSER_PORTS.write().unwrap_or_else(|p| p.into_inner());
    if let Some(set) = m.remove(workspace) {
        if !set.is_empty() {
            tracing::info!(workspace, ports = ?set, "proxy: browser-served ports revoked (REPL child gone)");
        }
    }
}

/// Parse the port out of a loopback `http(s)://` URL — `None` for any
/// non-loopback host (never allowlist an external address). Mirrors the FE's
/// `proxy_port_from_url`.
pub fn loopback_port_from_url(url: &str) -> Option<u16> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let (host, port) = authority.rsplit_once(':')?;
    if host != "127.0.0.1" && host != "localhost" {
        return None;
    }
    port.parse().ok()
}

/// The set of loopback ports `proxy.connect` will dial — the daemon's own
/// served HTTP surface. Computed per request so a runtime-assigned port is
/// honored without a restart.
///
/// VERIFIED-BOUND, not merely configured (closes the former "residual"
/// note): a preferred port whose bind lost the race belongs to some OTHER
/// process — on a shared host, typically another USER's daemon (the
/// 2026-07-23 shared-host collision) — and authorizing it would make this proxy
/// pipe the user's browser (URL tokens included) to a stranger's server.
///
/// - video: `bound_video_port()` (actual, ephemeral-fallback aware),
/// - docs shared: `bound_site_port()` (same),
/// - the docs pool's currently-ASSIGNED ports (`pool_assigned_ports()` —
///   actual bound ports, ephemeral-fallback aware) (codex),
/// - Pluto: `bound_pluto_port()` — the port parsed from the sidecar's READY
///   line; absent until the sidecar's first spawn (nothing to reach before
///   then, so nothing is authorized),
/// - WGL/Bonito + user-served dashboards: the per-workspace ports REPL
///   children announced via `browser` frames (`BROWSER_PORTS`, above) —
///   recorded when the BrowserView URL passes through the supervisor,
///   revoked when the child dies. This replaced the static `SOT_WGL_PORT`
///   entry, which was the last configured-not-verified port (the wglshow
///   flow records the port strictly before any browser can dial it),
/// - `SOT_PROXY_EXTRA_PORTS` (comma-separated) — escape hatch so an exotic
///   serving flow needs no daemon release.
pub fn allowed_proxy_ports() -> BTreeSet<u16> {
    let mut ports = BTreeSet::new();
    if let Some(p) = crate::pluto::bound_pluto_port() {
        ports.insert(p);
    }
    if let Some(p) = crate::http_serve::bound_video_port() {
        ports.insert(p);
    }
    if let Some(p) = crate::site_serve::bound_site_port() {
        ports.insert(p);
    }
    ports.extend(crate::site_serve::pool_assigned_ports());
    {
        let m = BROWSER_PORTS.read().unwrap_or_else(|p| p.into_inner());
        for set in m.values() {
            ports.extend(set.iter().copied());
        }
    }
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
    fn allowlist_is_verified_bound_not_configured() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("SOT_PROXY_EXTRA_PORTS");
        std::env::remove_var("SOT_WGL_PORT");
        let ports = allowed_proxy_ports();
        // In a process where no server has spawned, the formerly-hardcoded
        // preferred ports are NOT authorized: an unbound preferred port may
        // belong to another USER's process on a shared host, and authorizing
        // it would pipe this user's browser (URL tokens included) to a
        // stranger's server (2026-07-23 shared-host incident).
        assert!(!ports.contains(&1234), "pluto only once READY-parsed");
        assert!(!ports.contains(&1235), "video only when verified-bound");
        assert!(!ports.contains(&1236), "docs only when verified-bound");
        // WGL is no longer statically allowed either: a REPL child's served
        // port is allowlisted only once its BrowserView frame announces it
        // (and revoked when the child dies). Fully verified-bound now.
        assert!(!ports.contains(&1241), "wgl only when announced by a live child");
        // A random unrelated port is NOT proxyable.
        assert!(!ports.contains(&8080));
        assert!(!ports.contains(&22));
        // NB: no assertion on pool ports (1237–1240) here — the docs pool is
        // allow-listed by ASSIGNED port (pool_assigned_ports, the tightening),
        // which reads the process-global POOL that site_serve's own pool_tests
        // mutate in parallel; asserting emptiness here would race them (codex).
        // The "assigned-only" behavior is covered by site_serve's pool_tests.
        // Similarly no assertion on the video/docs BOUND statics — the bind
        // fallback tests in http_serve/site_serve set them in parallel; the
        // fixed-port absences above are stable (ephemeral binds land ≥32768).
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
    fn browser_ports_record_revoke_and_allowlist() {
        // Unique workspaces + high ports so parallel tests can't collide.
        record_browser_port("wsA-test", 45911);
        record_browser_port("wsA-test", 45912);
        record_browser_port("wsB-test", 45913);
        let ports = allowed_proxy_ports();
        assert!(ports.contains(&45911) && ports.contains(&45912), "wsA announced");
        assert!(ports.contains(&45913), "wsB announced");
        // Revoking one workspace drops exactly its grants.
        revoke_browser_ports("wsA-test");
        let ports = allowed_proxy_ports();
        assert!(!ports.contains(&45911) && !ports.contains(&45912), "wsA revoked");
        assert!(ports.contains(&45913), "wsB survives wsA's revoke");
        revoke_browser_ports("wsB-test");
        assert!(!allowed_proxy_ports().contains(&45913));
        // Per-workspace cap: 20 inserts keep at most BROWSER_PORTS_PER_WS.
        for p in 0..20u16 {
            record_browser_port("wsCap-test", 46000 + p);
        }
        let n = allowed_proxy_ports().iter().filter(|p| (46000..46020).contains(*p)).count();
        assert!(n <= BROWSER_PORTS_PER_WS, "cap enforced, kept {n}");
        revoke_browser_ports("wsCap-test");
    }

    #[test]
    fn loopback_port_from_url_is_loopback_only() {
        assert_eq!(loopback_port_from_url("http://127.0.0.1:1241/"), Some(1241));
        assert_eq!(loopback_port_from_url("http://localhost:45817/app?x=1"), Some(45817));
        assert_eq!(loopback_port_from_url("https://127.0.0.1:9000"), Some(9000));
        // Never allowlist an external host, a portless URL, or garbage.
        assert_eq!(loopback_port_from_url("http://10.0.0.5:1241/"), None);
        assert_eq!(loopback_port_from_url("http://example.com:80/"), None);
        assert_eq!(loopback_port_from_url("http://127.0.0.1/"), None);
        assert_eq!(loopback_port_from_url("file:///tmp/x"), None);
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

    /// The teardown regression (codex): upstream closes (EOF) while the CLIENT
    /// keeps its write half open forever. A `join!` teardown would wait for
    /// BOTH copies and hang here (the client→upstream copy never sees EOF); the
    /// `select!` teardown returns as soon as the upstream→client copy finishes.
    /// Asserting handle_proxy_connect returns within a timeout is what catches
    /// a regression back to `join!`.
    #[tokio::test]
    async fn upstream_close_tears_down_without_waiting_for_client() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Upstream that EOFs immediately on connect (a server that closed after
        // responding). Drop the accepted socket → the daemon's upstream read
        // sees EOF.
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                drop(s);
            }
        });
        std::env::set_var("SOT_PROXY_EXTRA_PORTS", port.to_string());

        let (client, daemon) = tokio::io::duplex(4096);
        let (dr, dw) = tokio::io::split(daemon);
        // The client NEVER closes its write half (`_cw` kept alive to scope
        // end), so the client→upstream direction never EOFs — the half-open
        // condition. `_cr` likewise kept so the duplex stays open.
        let (_cr, _cw) = tokio::io::split(client);

        let daemon_fut = handle_proxy_connect(codec::buffered(dr), dw, connect_frame(port), None);
        let res = tokio::time::timeout(std::time::Duration::from_secs(3), daemon_fut).await;
        assert!(
            res.is_ok(),
            "proxy did not tear down on upstream close while client stayed open (join! leak?)"
        );
        res.unwrap().unwrap();

        std::env::remove_var("SOT_PROXY_EXTRA_PORTS");
        drop(_cr);
        drop(_cw);
    }
}
