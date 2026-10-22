//! Daemon TCP proxy (ADR 0035).
//!
//! A remote frontend can reach ANY backend-served loopback HTTP/WebSocket
//! port through the ONE control tunnel it already holds, instead of a static per-port ssh `-L` forward per
//! service (Pluto 1234, video 1235, docs 1236, docs pool 1237-1240,
//! WGLMakie/Bonito 1241). This retires the launcher `-L` sprawl and the
//! recurring "box's launcher predates a port → dead page until relaunch"
//! failure class.
//!
//! A proxy connection is DEDICATED: the client opens a fresh daemon-socket
//! connection and sends a `handoff` hello and then `proxy.connect { port }`. If the
//! port is allowed and dials, the daemon answers `{ok:true}` and then pipes
//! every subsequent byte verbatim in both directions (`copy_bidirectional`)
//! until either side closes — which is exactly what carries the WebSocket
//! upgrade unmodified. It never joins the multiplexed control loop, so it
//! can't head-of-line-block pty/repl traffic and needs no flow control of
//! its own (native TCP backpressure).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, RwLock};

use anyhow::Result;
use sot_protocol::{codec, op, Frame, ProxyConnectReq};
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::sidecars::ports::{PortEvent, PortGrants, PortRefused};

/// Loopback ports announced by REPL children via their `browser` frames
/// (ADR 0032 `BrowserView` — `wglshow` and user-served dashboards), keyed by
/// owner (the row's id) so a dead child's grants die with it. This closes the last
/// configured-not-verified allowlist entry: the WGL/Bonito server binds
/// lazily inside the user's REPL child where the daemon can't observe the
/// bind, but the `BrowserView` URL it produces flows THROUGH the daemon as a
/// repl frame on its way to the FE — the supervisor announces the port off
/// that frame (`PortGrants`, `sidecars/repl/supervisor.rs`), so the allowlist learns the ACTUAL bound port
/// (ephemeral-fallback aware) with no protocol change and no trust in a
/// static port number.
///
/// Each owner holds the newest child generation that has announced anything and that generation's ports: an event of
/// a lower generation is refused (`PortRefused::Stale`), so an older child cannot grant or erase for its replacement,
/// and an event of a higher one first drops what the older child held. An owner's entry stays, empty, after its
/// `End`, to refuse the older child's late events; the table holds one per owner that has announced, until the
/// daemon ends.
///
/// Trust: the REPL child is the user's own arbitrary code, and the proxy
/// client is the same authenticated user — user code authorizing a loopback
/// port for the user's own browser adds no privilege over what either side
/// can already do (same standing as `SOT_PROXY_EXTRA_PORTS`).
static BROWSER_PORTS: RwLock<BTreeMap<String, BrowserGrants>> = RwLock::new(BTreeMap::new());

/// One owner's newest generation and its announced ports.
#[derive(Default)]
struct BrowserGrants {
    generation: u64,
    ports: BTreeSet<u16>,
}

/// Ports refused since they last passed the allowlist. The refusal is logged once per streak (a tab left open on a
/// dead page retries every second or two, forever), and a streak ends when the port is served again.
static REFUSED_PORTS: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());

/// Record whether `port` passed the allowlist; true when this refusal starts a streak.
fn refusal_starts_streak(port: u16, refused: bool) -> bool {
    let mut set = REFUSED_PORTS.lock().unwrap_or_else(|p| p.into_inner());
    if refused {
        set.insert(port)
    } else {
        set.remove(&port);
        false
    }
}

/// Per-workspace cap on announced ports — a backstop against a runaway loop
/// serving in a hot loop, not a real limit (a workspace realistically holds
/// one or two live served pages).
const BROWSER_PORTS_PER_WS: usize = 16;

/// Record a `browser`-frame loopback port for `owner`'s child of `generation`, or refuse it as stale.
fn allow_browser_port(owner: &str, generation: u64, port: u16) -> Result<(), PortRefused> {
    let mut m = BROWSER_PORTS.write().unwrap_or_else(|p| p.into_inner());
    let grants = m.entry(owner.to_string()).or_default();
    if generation < grants.generation {
        return Err(PortRefused::Stale { current: grants.generation });
    }
    if generation > grants.generation {
        grants.ports.clear();
        grants.generation = generation;
    }
    if grants.ports.insert(port) {
        while grants.ports.len() > BROWSER_PORTS_PER_WS {
            // BTreeSet has no insertion order; evicting the smallest is an
            // arbitrary-but-deterministic backstop.
            let evict = *grants.ports.iter().next().expect("non-empty set");
            grants.ports.remove(&evict);
        }
        tracing::info!(owner, port, "proxy: browser-served port recorded (allowlisted)");
    }
    Ok(())
}

/// Drop every announced port for `owner` and keep its generation, or refuse the `End` as stale. Called when its REPL
/// child dies (gen-guarded) and when a respawn begins — a freed port could be re-bound by any other process, so a
/// dead child's grant must not outlive it.
fn end_browser_ports(owner: &str, generation: u64) -> Result<(), PortRefused> {
    let mut m = BROWSER_PORTS.write().unwrap_or_else(|p| p.into_inner());
    let grants = m.entry(owner.to_string()).or_default();
    if generation < grants.generation {
        return Err(PortRefused::Stale { current: grants.generation });
    }
    grants.generation = generation;
    if !grants.ports.is_empty() {
        tracing::info!(owner, ports = ?grants.ports, "proxy: browser-served ports revoked (REPL child gone)");
        grants.ports.clear();
    }
    Ok(())
}

/// The receiver of the REPL's port announcements (`sidecars::ports`), which the daemon installs at its start.
pub struct PagesGrants;

impl PortGrants for PagesGrants {
    fn apply(&self, event: PortEvent) -> Result<(), PortRefused> {
        match event {
            PortEvent::Allow { owner, generation, port } => allow_browser_port(&owner, generation, port),
            PortEvent::End { owner, generation } => end_browser_ports(&owner, generation),
        }
    }
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
    if let Some(p) = crate::sidecars::pluto::bound_pluto_port() {
        ports.insert(p);
    }
    if let Some(p) = crate::pages::video::bound_video_port() {
        ports.insert(p);
    }
    if let Some(p) = crate::pages::site::bound_site_port() {
        ports.insert(p);
    }
    ports.extend(crate::pages::site::pool_assigned_ports());
    {
        let m = BROWSER_PORTS.read().unwrap_or_else(|p| p.into_inner());
        for grants in m.values() {
            ports.extend(grants.ports.iter().copied());
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

/// The dial to a served page port on 127.0.0.1. Made with tokio's `TcpSocket`, whose Windows socket is not inheritable,
/// where `TcpStream::connect` makes one that is: a child process the daemon starts while the connection is open must
/// not hold it.
async fn dial_upstream(port: u16) -> std::io::Result<TcpStream> {
    let socket = tokio::net::TcpSocket::new_v4()?;
    #[allow(clippy::disallowed_methods, reason = "the page plane: the daemon's proxy dials a page port it serves on loopback (ADR 0049, User isolation)")]
    let stream = socket.connect(std::net::SocketAddr::from(([127, 0, 0, 1], port))).await;
    stream
}

/// Handle a connection whose frame behind its `handoff` hello was `proxy.connect` (ADR 0035).
/// `rx` is the buffered reader that already consumed that frame (any
/// bytes it buffered past the envelope are preserved — `copy` drains the
/// BufReader before touching the socket); `tx` is the write half; `frame` is
/// the parsed handshake frame. Returns when the pipe closes; errors are logged by the
/// caller.
pub async fn handle_proxy_connect<R, W>(
    rx: R,
    mut tx: W,
    frame: Frame,
) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let req: ProxyConnectReq = match serde_json::from_value(frame.payload) {
        Ok(r) => r,
        Err(e) => {
            return reject(&mut tx, frame.id, op::PROXY_CONNECT, "bad_request", &format!("{e}")).await;
        }
    };

    // Loopback-only + served-port allowlist. The frame carries no host by
    // design; we dial 127.0.0.1 exclusively, so a compromised/hostile FE
    // can only reach the daemon's OWN loopback services, never arbitrary
    // internal hosts.
    if !allowed_proxy_ports().contains(&req.port) {
        if refusal_starts_streak(req.port, true) {
            tracing::warn!(port = req.port, "proxy.connect rejected: port not in allowlist (logged once until it is served again)");
        }
        return reject(
            &mut tx,
            frame.id,
            op::PROXY_CONNECT,
            "bad_port",
            &format!("port {} is not a proxyable backend port", req.port),
        )
        .await;
    }
    refusal_starts_streak(req.port, false);

    // Dial the backend service. A short connect timeout keeps a wedged
    // target from parking the proxy task forever.
    let dial = tokio::time::timeout(std::time::Duration::from_secs(5), dial_upstream(req.port)).await;
    let upstream = match dial {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(port = req.port, error = %e, "proxy.connect dial failed");
            return reject(&mut tx, frame.id, op::PROXY_CONNECT, "dial_failed", &format!("{e}")).await;
        }
        Err(_) => {
            tracing::warn!(port = req.port, "proxy.connect dial timed out");
            return reject(&mut tx, frame.id, op::PROXY_CONNECT, "dial_failed", "connect timed out").await;
        }
    };
    let _ = upstream.set_nodelay(true);

    // Handshake OK → hand the connection over to a raw bidirectional pipe.
    let res = Frame::res(
        frame.id,
        op::PROXY_CONNECT,
        serde_json::json!({ "ok": true }),
    );
    codec::write_frame(&mut tx, &res, None).await?; // flushes internally
    tracing::info!(port = req.port, "proxy.connect established — piping");

    pipe_bidirectional(rx, tx, upstream, &format!("port {}", req.port)).await
}

use sot_protocol::pipe::pipe_bidirectional;
use sot_protocol::reply::reject;

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
