//! Frontend half of the daemon TCP proxy (ADR 0035), C3 as amended §3.
//!
//! A REMOTE frontend reaches any backend-served loopback page (Pluto, video,
//! docs + pool, WGLMakie/Bonito) through its OWNING daemon: the window's page proxy opens a dedicated SSH or
//! generated-relay connection using the owning host's resolved control selection; handoff hello and proxy.connect
//! share one write. There is no per-port ssh `-L` forward and no launcher edit when a new
//! backend port appears. A multi-host FE makes one such connection per browser
//! connection (W2, the accepted cost — isolation-plan.md §10), so each
//! listener carries its OWN target (never one baked-in default-host
//! address for every port — that was the cross-host figure defect: a page
//! served by a non-default host's daemon had nowhere to proxy through). The
//! browser still opens a plain `http://127.0.0.1:<port>/…` URL; for a page a
//! daemon op served its port is this window's listener's, bound where the OS
//! assigns it, not the daemon's (PAGE-PORT, `PageSource`); the listener pipes
//! each browser connection to the right daemon, which dials the real service
//! (the daemon half validates the port + does the dialing —
//! `backend/src/pages/proxy.rs`).
//!
//! Ownership split that keeps the "bind before the browser launches" ordering
//! honest without blocking the render thread: the GPU thread binds a
//! `std::net::TcpListener` SYNCHRONOUSLY (a bind is sub-millisecond, no
//! `block_on`, so the port is already listening the instant
//! `browser_open::open_page` runs) and hands the bound listener — tagged with the
//! ssh recipe and token it resolved for that page's host — to the transport
//! runtime here, which owns the async accept loop + the per-connection pipe.
//!
//! Part of pages; charter: rust/backend/src/pages/CLAUDE.md.

use std::net::TcpListener as StdTcpListener;

use sot_protocol::topology::ssh_bridge::{LinkGate, LinkWatch, SpawnError, SshRecipe};
use sot_protocol::{codec, op, Frame, HelloReq, ProxyConnectReq, HANDOFF_ROLE};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedReceiver;

/// Where a listener's browser connections go to reach their page's owning daemon: a dedicated ssh login, or a
/// generated hub-relay socket reached locally. Both end in the same handoff hello and `proxy.connect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageDial {
    Ssh(SshRecipe),
    Relay(std::path::PathBuf),
}

impl std::fmt::Display for PageDial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PageDial::Ssh(recipe) => recipe.fmt(f),
            PageDial::Relay(path) => write!(f, "relay:{}", path.display()),
        }
    }
}

/// Whether an armed port's listener dials the daemon, shared by the GPU thread, which opens pages, and that port's
/// listener. Even: dialing. Odd: parked — the daemon answered `bad_port`, so browser connections close at the
/// listener with no ssh login until a page on the port is opened again. Each open moves to the next even value, so a
/// refusal of a dial made before that open cannot park the port, and two refusals of one dial park (and log) once.
#[derive(Default)]
pub struct Arm(std::sync::atomic::AtomicU64);

impl Arm {
    /// A page on this port was opened (again): dial from now on.
    pub fn reopen(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        let _ = self.0.fetch_update(SeqCst, SeqCst, |s| Some((s | 1) + 1));
    }

    /// The state a new browser connection dials under, or `None` while parked.
    fn dial(&self) -> Option<u64> {
        let s = self.0.load(std::sync::atomic::Ordering::SeqCst);
        (s % 2 == 0).then_some(s)
    }

    /// The daemon refused the dial made under `s`; true only for the refusal that parks the port.
    fn refused(&self, s: u64) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.0.compare_exchange(s, s + 1, SeqCst, SeqCst).is_ok()
    }
}

/// How the daemon answered one browser connection's `proxy.connect`; every other end is an `Err`.
#[derive(Debug)]
enum Answer {
    /// `{ok: true}`: the connection was piped until either side closed.
    Piped,
    /// The typed `bad_port` refusal: the daemon does not serve this port, so its listener parks it (`Arm`).
    NotServed,
    /// The host's link is down: no ssh child was spawned and the browser connection closes at once. The frontend's
    /// status line already says the link is down, so this writes no log line (ADR 0045 decision 4).
    LinkDown,
}

/// One armed page port, as the GPU thread hands it to the manager: this window's bound `listener`, and the
/// `daemon_port` its browser connections reach. They differ: the listener's port is the window's computer's, the
/// daemon's port is the daemon's computer's (PAGE-PORT), and `proxy.connect` names the daemon's.
pub struct PageListener {
    pub listener: StdTcpListener,
    pub daemon_port: u16,
    pub dial: PageDial,
    pub token: Option<String>,
    pub gate: LinkGate,
    pub arm: std::sync::Arc<Arm>,
}

/// Spawn the proxy manager on the transport runtime. It receives bound
/// listeners from the GPU thread (`State::ensure_proxy_for_url`), each
/// tagged with its daemon port and `(dial, token)` — the exact recipe and token resolved
/// for the page's OWNING host, not a single manager-wide default. Per
/// listener it runs an accept loop that pipes each accepted browser
/// connection to a FRESH connection made from that listener's own
/// `dial`; `token` is forwarded in the handshake when that daemon has one
/// configured (Unix-socket transports carry none). A port the daemon answers `bad_port` is parked (`Arm`) until a page
/// on it is opened again; the listener itself never closes.
pub fn spawn_proxy_manager(rt: &tokio::runtime::Runtime, mut listener_rx: UnboundedReceiver<PageListener>) {
    rt.spawn(async move {
        while let Some(armed) = listener_rx.recv().await {
            let PageListener { listener: std_listener, daemon_port: port, dial: target, token, gate, arm } = armed;
            // The GPU thread already set it non-blocking; from_std needs that.
            let listener = match tokio::net::TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(port, error = %e, "proxy: from_std failed; dropping listener");
                    continue;
                }
            };
            tracing::info!(port, %target, "proxy: accepting browser connections for backend port");
            tokio::spawn(serve_page_listener(listener, port, target, token, gate, arm));
        }
        tracing::debug!("proxy: listener channel closed; manager exiting");
    });
}

/// Serve one armed listener: each browser connection `serve_own` admits is piped to `dial`'s daemon with
/// `proxy.connect` naming `port`, the daemon's page port, never the listener's own.
async fn serve_page_listener(
    listener: tokio::net::TcpListener,
    port: u16,
    target: PageDial,
    token: Option<String>,
    gate: LinkGate,
    arm: std::sync::Arc<Arm>,
) {
    let dial = move |browser| {
        let (d, g, t) = (target.clone(), gate.clone(), token.clone());
        async move { pipe_one(browser, &d, &g, port, t.as_deref()).await }
    };
    sot_log::identity::peer_owner::serve_own(listener, "page-proxy", move |browser| {
        let arm = std::sync::Arc::clone(&arm);
        let done = dial(browser);
        async move { serve_browser(port, &arm, done).await }
    })
    .await
}

/// One browser connection `serve_own` has admitted, as `done`, the future of its `dial`. A port the daemon refused
/// (`Arm`) is parked: the connection is dropped here, with no ssh login and no log line. Otherwise `dial` runs and its
/// answer is logged. The listener never closes, so the port stays this frontend's for its whole life.
async fn serve_browser<Fut>(port: u16, arm: &Arm, done: Fut)
where
    Fut: std::future::Future<Output = anyhow::Result<Answer>>,
{
    let Some(armed) = arm.dial() else {
        return; // parked: `done` drops unpolled, closing the connection at once
    };
    match done.await {
        Ok(Answer::Piped | Answer::LinkDown) => {}
        Ok(Answer::NotServed) => {
            if arm.refused(armed) {
                tracing::warn!(port, "proxy: the daemon does not serve this port (bad_port); parked: browser connections to it close here until the page is opened again");
            }
        }
        // A dead child here is a blank page with no
        // other carrier — `debug!` → `warn!` (C3 as
        // amended §6): a reason at `debug` is
        // invisible in a normal run.
        Err(e) => tracing::warn!(port, error = %e, "proxy: connection ended"),
    }
}

/// Pipe one browser connection to the daemon that owns `port`'s page (C3 as amended §3): reach it through a
/// fresh ssh child spawned from the recipe, or through the generated relay socket's own protected connection, do
/// the `proxy.connect` handshake behind a handoff hello as the first bytes (`server/conn.rs` `hand_off`), then
/// splice bytes both ways until either side closes (carrying a WebSocket upgrade verbatim). While the host's link
/// is down (`gate`) nothing is spawned or connected and the browser connection closes at once; the gate watches the
/// login from before it starts until its replies, so a close of the link ends it the same way.
async fn pipe_one(
    browser: tokio::net::TcpStream,
    target: &PageDial,
    gate: &LinkGate,
    port: u16,
    token: Option<&str>,
) -> anyhow::Result<Answer> {
    let Some(link) = gate.watch() else {
        return Ok(Answer::LinkDown);
    };
    match target {
        PageDial::Ssh(recipe) => {
            #[allow(
                clippy::disallowed_methods,
                reason = "the window's page-proxy ssh, owned by the window"
            )]
            let child = match gate.spawn_async(recipe) {
                Ok(child) => child,
                Err(SpawnError::LinkDown) => return Ok(Answer::LinkDown),
                Err(e) => return Err(anyhow::Error::new(e).context(format!("spawn ssh {recipe}"))),
            };
            pipe_child(child, browser, port, token, Some(link)).await
        }
        PageDial::Relay(path) => {
            use interprocess::local_socket::tokio::prelude::*;
            let stream = crate::net::transport::connect_pipe(path).await?;
            let (rx, mut tx) = stream.split();
            let mut rx = codec::buffered(rx);
            let _ = browser.set_nodelay(true);
            pipe_one_over(&mut tx, &mut rx, browser, port, token, Some(link)).await
        }
    }
}

/// The rest of [`pipe_one`] over a spawned child (an ssh login, or in a test a stand-in): the handshake, the splice,
/// and, when it fails, the child's last stderr line after the error's own words.
async fn pipe_child(
    mut child: tokio::process::Child,
    browser: tokio::net::TcpStream,
    port: u16,
    token: Option<&str>,
    link: Option<LinkWatch>,
) -> anyhow::Result<Answer> {
    let d_wr = child.stdin.take().expect("spawned with a piped stdin");
    let d_rd = child.stdout.take().expect("spawned with a piped stdout");
    let stderr = child.stderr.take().expect("spawned with a piped stderr");
    let last_stderr = crate::net::transport::spawn_stderr_drain(stderr);
    let mut d_wr = d_wr;
    let _ = browser.set_nodelay(true);

    // Handshake read goes through a BufReader; the pipe below copies FROM that
    // same BufReader so any bytes it buffered past the response envelope (the
    // daemon may start streaming upstream data immediately after `{ok}`) are
    // not lost — the throwaway-reader trap.
    let mut d_buf = BufReader::new(d_rd);
    let result = pipe_one_over(&mut d_wr, &mut d_buf, browser, port, token, link).await;
    // `child` drops here (`kill_on_drop`), ending this connection's ssh
    // login. The child's last stderr line is added after the error's own words when it died
    // before or during the splice: the daemon's answer (a refused hello) stays first.
    match result {
        Err(e) => match sot_protocol::topology::ssh_bridge::last_stderr_after_failure(&last_stderr).await {
            Some(line) => Err(anyhow::anyhow!("{e:#}: {line}")),
            None => Err(e),
        },
        ok => ok,
    }
}

async fn pipe_one_over<W, R>(
    d_wr: &mut W,
    d_buf: &mut R,
    browser: tokio::net::TcpStream,
    port: u16,
    token: Option<&str>,
    link: Option<LinkWatch>,
) -> anyhow::Result<Answer>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    let req = ProxyConnectReq {
        port,
        token: token.map(|s| s.to_string()),
    };
    // The handoff hello (ADR 0049 `## User isolation`) and `proxy.connect` go out in one write, so the hello costs
    // no round trip; the two replies come back through the one reader.
    let hello = HelloReq::this_process("sot-fe-proxy", HANDOFF_ROLE, Some(crate::net::identity::frontend_identity().host.clone()))?;
    let login = async {
        let mut both = Vec::new();
        codec::write_frame(&mut both, &Frame::req(1, op::HELLO, serde_json::to_value(&hello)?), None).await?;
        codec::write_frame(&mut both, &Frame::req(2, op::PROXY_CONNECT, serde_json::to_value(&req)?), None).await?;
        d_wr.write_all(&both).await?;
        d_wr.flush().await?;

        // Each reply within the connect bound: a daemon that never answers names the step, not a page that loads forever.
        let (hello_res, _blob) = crate::net::transport::read_connect_reply(&mut *d_buf, op::HELLO).await?;
        if hello_res.payload.get("error").is_some() {
            let code = hello_res.payload.get("code").and_then(|v| v.as_str()).unwrap_or("error");
            anyhow::bail!("daemon refused the hello for port {port}: {code}");
        }
        let (res, _blob) = crate::net::transport::read_connect_reply(&mut *d_buf, op::PROXY_CONNECT).await?;
        anyhow::Ok(res)
    };
    // The login ends with the host's link (`LinkWatch`); a page already piped does not.
    let res = match link {
        Some(mut link) => tokio::select! {
            res = login => res?,
            () = link.closed() => return Ok(Answer::LinkDown),
        },
        None => login.await?,
    };
    if res.payload.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let code = res
            .payload
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        if code == "bad_port" {
            return Ok(Answer::NotServed);
        }
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
    Ok(Answer::Piped)
}

/// Write `html_bytes` to a unique temp file and hand it off to the OS
/// default browser via `crate::browser_open::spawn_opener`. We don't delete the temp
/// file (the OS cleans temp on its own schedule; a fresh path per call
/// also prevents the browser from showing a stale cached version).
pub(crate) fn open_html_in_browser(html_bytes: &[u8]) -> std::io::Result<()> {
    let path = write_preview_html(&std::env::temp_dir(), html_bytes)?;
    crate::browser_open::spawn_opener(&path.to_string_lossy())
}

/// A new file for a rendered preview in `dir`: created, never overwritten, and readable by this account only on Unix,
/// where `/tmp` is shared by every account on the box (ADR 0049, User isolation).
fn write_preview_html(dir: &std::path::Path, html_bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("sot-preview-{now}.html"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&path)?.write_all(html_bytes)?;
    Ok(path)
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod tests;
