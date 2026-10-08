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
//! browser still opens a plain `http://127.0.0.1:<port>/…` URL; this module
//! makes that loopback port resolve by binding a local listener that pipes
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

use sot_protocol::topology::ssh_bridge::{LinkGate, SpawnError, SshRecipe};
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

/// Spawn the proxy manager on the transport runtime. It receives bound
/// listeners from the GPU thread (`State::ensure_proxy_for_url`), each
/// tagged with `(recipe, token)` — the exact ssh recipe and token resolved
/// for the page's OWNING host, not a single manager-wide default. Per
/// listener it runs an accept loop that pipes each accepted browser
/// connection to a FRESH ssh child spawned from that listener's own
/// `recipe`; `token` is forwarded in the handshake when that daemon has one
/// configured (Unix-socket transports carry none). A port the daemon answers `bad_port` is parked (`Arm`) until a page
/// on it is opened again; the listener itself never closes.
pub fn spawn_proxy_manager(
    rt: &tokio::runtime::Runtime,
    mut listener_rx: UnboundedReceiver<(
        StdTcpListener,
        PageDial,
        Option<String>,
        LinkGate,
        std::sync::Arc<Arm>,
    )>,
) {
    rt.spawn(async move {
        while let Some((std_listener, target, token, gate, arm)) = listener_rx.recv().await {
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
            tracing::info!(port, %target, "proxy: accepting browser connections for backend port");
            let dial = move |browser| {
                let (d, g, t) = (target.clone(), gate.clone(), token.clone());
                async move { pipe_one(browser, &d, &g, port, t.as_deref()).await }
            };
            tokio::spawn(sot_log::identity::peer_owner::serve_own(listener, "page-proxy", move |browser| {
                let arm = std::sync::Arc::clone(&arm);
                let done = dial(browser);
                async move { serve_browser(port, &arm, done).await }
            }));
        }
        tracing::debug!("proxy: listener channel closed; manager exiting");
    });
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
/// is down (`gate`) nothing is spawned or connected and the browser connection closes at once.
async fn pipe_one(
    browser: tokio::net::TcpStream,
    target: &PageDial,
    gate: &LinkGate,
    port: u16,
    token: Option<&str>,
) -> anyhow::Result<Answer> {
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
            pipe_child(child, browser, port, token).await
        }
        PageDial::Relay(path) => {
            if !gate.is_up() {
                return Ok(Answer::LinkDown);
            }
            use interprocess::local_socket::tokio::prelude::*;
            let stream = crate::net::transport::connect_pipe(path).await?;
            let (rx, mut tx) = stream.split();
            let mut rx = codec::buffered(rx);
            let _ = browser.set_nodelay(true);
            pipe_one_over(&mut tx, &mut rx, browser, port, token).await
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
    let result = pipe_one_over(&mut d_wr, &mut d_buf, browser, port, token).await;
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
    let mut both = Vec::new();
    codec::write_frame(&mut both, &Frame::req(1, op::HELLO, serde_json::to_value(&hello)?), None).await?;
    codec::write_frame(&mut both, &Frame::req(2, op::PROXY_CONNECT, serde_json::to_value(&req)?), None).await?;
    d_wr.write_all(&both).await?;
    d_wr.flush().await?;

    let (hello_res, _blob) = codec::read_frame(d_buf).await?;
    if hello_res.payload.get("error").is_some() {
        let code = hello_res.payload.get("code").and_then(|v| v.as_str()).unwrap_or("error");
        anyhow::bail!("daemon refused the hello for port {port}: {code}");
    }
    let (res, _blob) = codec::read_frame(d_buf).await?;
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
mod tests {
    /// A proxy listener as production runs it: `serve_own` with the real owner check, each admitted connection handed to
    /// `serve_browser` with the future of its `dial` (ADR 0049, User isolation).
    fn serve<D, Fut>(
        listener: tokio::net::TcpListener,
        port: u16,
        arm: std::sync::Arc<super::Arm>,
        dial: D,
    ) -> tokio::task::JoinHandle<()>
    where
        D: Fn(tokio::net::TcpStream) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<super::Answer>> + Send + 'static,
    {
        tokio::spawn(sot_log::identity::peer_owner::serve_own(listener, "page-proxy", move |browser| {
            let arm = std::sync::Arc::clone(&arm);
            let done = dial(browser);
            async move { super::serve_browser(port, &arm, done).await }
        }))
    }

    /// ADR 0045 decision 4: with the host's link down a browser connection
    /// closes at once with `Answer::LinkDown` and no ssh child is spawned.
    #[tokio::test]
    async fn a_down_gate_closes_every_browser_connection_without_a_spawn() {
        use tokio::io::AsyncReadExt;
        let gate = sot_protocol::topology::ssh_bridge::LinkGate::default();
        gate.set_up(false);
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        for _ in 0..5 {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (browser, _) = listener.accept().await.unwrap();
            let t0 = std::time::Instant::now();
            let answer = super::pipe_one(
                browser,
                &super::PageDial::Ssh(recipe.clone()),
                &gate,
                addr.port(),
                None,
            )
            .await;
            assert!(
                matches!(answer, Ok(super::Answer::LinkDown)),
                "got {answer:?}"
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

    /// A down link never parks the port and writes no warning: every connection made while it is down still
    /// reaches `dial`, and the frontend log stays quiet. A seventh connection whose dial fails is the control: that
    /// warning must appear, so the capture is proven to see the listener's warnings.
    #[tokio::test]
    async fn a_down_link_never_parks_the_port() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // #[tokio::test] runs every task on this thread, so the capture sees the listener's spawned tasks.
        let log = sot_log::test_log::capture();
        let warns = || log.text().lines().filter(|l| l.contains(" WARN ")).count();
        let dials = Arc::new(AtomicUsize::new(0));
        let arm = Arc::new(super::Arm::default());
        let dial = {
            let dials = dials.clone();
            move |_browser: tokio::net::TcpStream| {
                let dials = dials.clone();
                async move {
                    if dials.fetch_add(1, SeqCst) == 6 {
                        return Err(anyhow::anyhow!("control: the ssh child died"));
                    }
                    Ok(super::Answer::LinkDown)
                }
            }
        };
        let task = serve(listener, port, arm.clone(), dial);
        for _ in 0..6 {
            let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let mut buf = [0u8; 1];
            let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf))
                .await
                .expect("the browser connection must end")
                .unwrap_or(0);
            assert_eq!(n, 0);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(dials.load(SeqCst), 6, "a link-down answer must not park the port");
        assert_eq!(warns(), 0, "a link-down retry wrote a warning");
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut buf = [0u8; 1];
        let _ = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf)).await;
        for _ in 0..100 {
            if warns() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(warns(), 1, "the control's failed dial must warn once");
        task.abort();
    }

    /// A port the daemon refuses with `bad_port` is parked: the listener stays bound, makes no further dial, and
    /// dials again once a page on the port is opened. The dial runs the REAL `pipe_one_over` handshake against an
    /// in-process daemon that writes the daemon's exact refusal frame.
    #[tokio::test]
    async fn a_bad_port_answer_parks_the_port_until_the_page_is_opened_again() {
        use sot_protocol::{codec, op, Frame};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let serving = Arc::new(AtomicBool::new(true));
        let dials = Arc::new(AtomicUsize::new(0));
        let arm = Arc::new(super::Arm::default());
        let dial = {
            let (serving, dials) = (serving.clone(), dials.clone());
            move |browser: tokio::net::TcpStream| {
                let (serving, dials) = (serving.clone(), dials.clone());
                async move {
                    dials.fetch_add(1, SeqCst);
                    let (fe, daemon) = tokio::io::duplex(4096);
                    let (d_rd, mut d_wr) = tokio::io::split(daemon);
                    tokio::spawn(async move {
                        let mut d_buf = tokio::io::BufReader::new(d_rd);
                        // The daemon's admission (ADR 0049 `## User isolation`): a handoff hello first, else
                        // `unauthenticated` and the end.
                        let (hello, _) = codec::read_frame(&mut d_buf).await.unwrap();
                        if hello.op != op::HELLO || hello.payload["role"] != "handoff" {
                            let refusal = serde_json::json!({ "error": "send a hello first", "code": "unauthenticated" });
                            codec::write_frame(&mut d_wr, &Frame::res(hello.id, &hello.op, refusal), None).await.unwrap();
                            return;
                        }
                        let accepted = serde_json::json!({ "session_id": "s", "revision": 0, "snapshot_pending": false });
                        codec::write_frame(&mut d_wr, &Frame::res(hello.id, op::HELLO, accepted), None).await.unwrap();
                        let (req, _) = codec::read_frame(&mut d_buf).await.unwrap();
                        assert_eq!(req.op, op::PROXY_CONNECT);
                        let payload = if serving.load(SeqCst) {
                            serde_json::json!({ "ok": true })
                        } else {
                            // What the daemon's `reject` writes for a port it does not serve (backend pages/proxy.rs).
                            serde_json::json!({ "error": format!("port {port} is not a proxyable backend port"), "code": "bad_port" })
                        };
                        codec::write_frame(&mut d_wr, &Frame::res(req.id, op::PROXY_CONNECT, payload), None).await.unwrap();
                        // Both daemon halves drop here, so a served connection ends at once.
                    });
                    let (f_rd, mut f_wr) = tokio::io::split(fe);
                    let mut f_buf = tokio::io::BufReader::new(f_rd);
                    super::pipe_one_over(&mut f_wr, &mut f_buf, browser, port, None).await
                }
            }
        };
        let task = serve(listener, port, arm.clone(), dial);
        let browse = || async move {
            let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let mut buf = [0u8; 1];
            let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf))
                .await
                .expect("the browser connection must end")
                .unwrap_or(0);
            assert_eq!(n, 0);
        };
        for _ in 0..3 {
            browse().await;
        }
        assert_eq!(dials.load(SeqCst), 3);
        assert!(arm.dial().is_some(), "a served port keeps dialing");
        serving.store(false, SeqCst);
        browse().await;
        let parked = tokio::time::timeout(Duration::from_secs(2), async {
            while arm.dial().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(parked.is_ok(), "a bad_port answer must park the port");
        for _ in 0..5 {
            browse().await;
        }
        assert_eq!(dials.load(SeqCst), 4, "a parked port makes no dial");
        serving.store(true, SeqCst);
        arm.reopen();
        browse().await;
        assert_eq!(dials.load(SeqCst), 5, "a re-opened page dials again");
        assert!(arm.dial().is_some());
        assert!(!task.is_finished(), "the listener never closes");
        task.abort();
    }

    /// A private folder and socket path for a stand-in relay: `connect_own` speaks only to a socket in a folder this
    /// account alone can enter.
    #[cfg(unix)]
    fn relay_stand_in_path(tag: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::path::PathBuf::from(format!("/tmp/sot-relay-page-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir.join("sot-host-far.sock")
    }

    /// One browser connection and its far end, as the proxy's accept loop hands them over.
    #[cfg(unix)]
    async fn browser_pair() -> (tokio::net::TcpStream, tokio::net::TcpStream) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (browser, _) = listener.accept().await.unwrap();
        (browser, client)
    }

    /// A page dial over a generated relay reaches the stand-in daemon with the handoff hello and `proxy.connect`, and
    /// the bytes written right behind the daemon's reply reach the browser whole; a `bad_port` answer is the usual
    /// refusal that parks the port.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_relay_page_dial_splices_after_handoff_and_reports_bad_port() {
        use sot_protocol::{codec, op, Frame};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (tag, serve_port) in [("ok", true), ("bad", false)] {
            let path = relay_stand_in_path(tag);
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let daemon = tokio::spawn(async move {
                let (conn, _) = listener.accept().await.unwrap();
                let (rd, mut wr) = conn.into_split();
                let mut rd = tokio::io::BufReader::new(rd);
                let (hello, _) = codec::read_frame(&mut rd).await.unwrap();
                assert_eq!(
                    (hello.op.as_str(), hello.payload["role"].as_str()),
                    (op::HELLO, Some("handoff"))
                );
                let (req, _) = codec::read_frame(&mut rd).await.unwrap();
                assert_eq!(req.op, op::PROXY_CONNECT);
                let accepted = serde_json::json!({ "session_id": "s", "revision": 0, "snapshot_pending": false });
                let reply = if serve_port {
                    serde_json::json!({ "ok": true })
                } else {
                    serde_json::json!({ "error": "not proxyable", "code": "bad_port" })
                };
                let mut out = Vec::new();
                codec::write_frame(&mut out, &Frame::res(hello.id, op::HELLO, accepted), None)
                    .await
                    .unwrap();
                codec::write_frame(
                    &mut out,
                    &Frame::res(req.id, op::PROXY_CONNECT, reply),
                    None,
                )
                .await
                .unwrap();
                out.extend_from_slice(b"page bytes right behind the reply");
                wr.write_all(&out).await.unwrap();
                wr.flush().await.unwrap();
                let mut rest = Vec::new();
                let _ = rd.read_to_end(&mut rest).await;
            });
            let (browser, mut client) = browser_pair().await;
            let gate = sot_protocol::topology::ssh_bridge::LinkGate::default();
            gate.set_up(true);
            let dial = tokio::spawn(async move {
                super::pipe_one(browser, &super::PageDial::Relay(path), &gate, 7000, None).await
            });
            if serve_port {
                let mut got = vec![0u8; "page bytes right behind the reply".len()];
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    client.read_exact(&mut got),
                )
                .await
                .expect("the page bytes must arrive")
                .unwrap();
                assert_eq!(got, b"page bytes right behind the reply");
                drop(client);
                let answer = tokio::time::timeout(std::time::Duration::from_secs(5), dial)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(matches!(answer, Ok(super::Answer::Piped)), "got {answer:?}");
            } else {
                let answer = tokio::time::timeout(std::time::Duration::from_secs(5), dial)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    matches!(answer, Ok(super::Answer::NotServed)),
                    "got {answer:?}"
                );
            }
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), daemon).await;
        }
    }

    /// With the host's link down, a relay page dial never connects: the stand-in sees no connection at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_down_gate_makes_no_relay_connection() {
        let path = relay_stand_in_path("down");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let gate = sot_protocol::topology::ssh_bridge::LinkGate::default();
        gate.set_up(false);
        for _ in 0..3 {
            let (browser, _client) = browser_pair().await;
            let answer = super::pipe_one(
                browser,
                &super::PageDial::Relay(path.clone()),
                &gate,
                7000,
                None,
            )
            .await;
            assert!(
                matches!(answer, Ok(super::Answer::LinkDown)),
                "got {answer:?}"
            );
        }
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(
            accepted.is_err(),
            "a down link must not connect to the relay"
        );
    }

    #[test]
    fn a_refusal_older_than_the_last_open_does_not_park() {
        let arm = super::Arm::default();
        let before = arm.dial().unwrap();
        arm.reopen(); // the page is opened again while that dial is in flight
        assert!(!arm.refused(before), "a refusal of a dial made before the open does not park");
        let now = arm.dial().expect("still dialing");
        assert!(arm.refused(now), "a refusal of a current dial parks");
        assert!(!arm.refused(now), "a second refusal of the same dial parks (and logs) once");
        assert!(arm.dial().is_none());
        arm.reopen();
        assert!(arm.dial().is_some(), "opening the page again un-parks");
    }

    /// ADR 0049, User isolation: a rendered preview in the shared temp folder is this account's alone, and a second
    /// preview is a new file, never an overwrite.
    #[test]
    fn a_preview_file_is_private_and_never_overwritten() {
        let dir = std::env::temp_dir().join(format!("sot-preview-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = super::write_preview_html(&dir, b"<p>one</p>").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = super::write_preview_html(&dir, b"<p>two</p>").unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"<p>one</p>");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in [&a, &b] {
                assert_eq!(std::fs::metadata(f).unwrap().permissions().mode() & 0o777, 0o600, "{f:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// A refused hello keeps the daemon's words first and takes the ssh login's last stderr line after them, never the
    /// line alone (BLOCKER 2 of review round 1). The login is `sh`, which writes the line and refuses the hello.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_hello_keeps_its_words_before_the_ssh_line() {
        use sot_protocol::{op, Frame};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _client = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (browser, _) = listener.accept().await.unwrap();
        let refusal = Frame::res(1, op::HELLO, serde_json::json!({ "error": "no thanks", "code": "os_user_conflict" }));
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("echo 'a line ssh wrote to stderr' >&2; read a; read b; printf '%s\\n' \"$REFUSAL\"")
            .env("REFUSAL", serde_json::to_string(&refusal).unwrap())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let err = super::pipe_child(child, browser, port, None).await.map(|_| ()).unwrap_err();
        assert_eq!(err.to_string(), format!("daemon refused the hello for port {port}: os_user_conflict: a line ssh wrote to stderr"));
    }
}
