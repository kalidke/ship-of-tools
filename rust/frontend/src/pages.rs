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
//! `backend/src/pages/proxy.rs`).
//!
//! Ownership split that keeps the "bind before the browser launches" ordering
//! honest without blocking the render thread: the GPU thread binds a
//! `std::net::TcpListener` SYNCHRONOUSLY (a bind is sub-millisecond, no
//! `block_on`, so the port is already listening the instant
//! `open_url_in_browser` runs) and hands the bound listener — tagged with the
//! ssh recipe and token it resolved for that page's host — to the transport
//! runtime here, which owns the async accept loop + the per-connection pipe.
//!
//! Part of pages; charter: rust/backend/src/pages/CLAUDE.md.

use std::net::TcpListener as StdTcpListener;

use sot_protocol::topology::ssh_bridge::{LinkGate, SpawnError, SshRecipe};
use sot_protocol::{codec, op, Frame, ProxyConnectReq};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedReceiver;

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
    mut listener_rx: UnboundedReceiver<(StdTcpListener, SshRecipe, Option<String>, LinkGate, std::sync::Arc<Arm>)>,
) {
    rt.spawn(async move {
        while let Some((std_listener, recipe, token, gate, arm)) = listener_rx.recv().await {
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
            tracing::info!(port, %recipe, "proxy: accepting browser connections for backend port");
            tokio::spawn(serve_listener(listener, port, arm, move |browser| {
                let (r, g, t) = (recipe.clone(), gate.clone(), token.clone());
                async move { pipe_one(browser, &r, &g, port, t.as_deref()).await }
            }));
        }
        tracing::debug!("proxy: listener channel closed; manager exiting");
    });
}

/// One listener's accept loop. Each browser connection runs `dial` unless the port is parked (`Arm`), in which case
/// it is dropped here: no ssh login, no log line. The listener never closes, so the port stays this frontend's for
/// its whole life, as it always has.
async fn serve_listener<D, Fut>(listener: tokio::net::TcpListener, port: u16, arm: std::sync::Arc<Arm>, dial: D)
where
    D: Fn(tokio::net::TcpStream) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<Answer>> + Send + 'static,
{
    loop {
        match listener.accept().await {
            Ok((browser, _peer)) => {
                let Some(armed) = arm.dial() else {
                    continue; // parked: `browser` drops here, closing the connection at once
                };
                let done = dial(browser);
                let arm = std::sync::Arc::clone(&arm);
                tokio::spawn(async move {
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
}

/// Pipe one browser connection through an ssh child spawned from `recipe`
/// for `port` (C3 as amended §3): spawn, do the `proxy.connect` handshake
/// as the child's first bytes — the daemon peeks that op on ANY accepted
/// connection (`server/conn.rs`), so the frames are byte-identical to the old
/// tcp-forwarded leg — then splice bytes both ways until either side closes
/// (carrying a WebSocket upgrade verbatim). While the host's link is down
/// (`gate`) no child is spawned and the browser connection closes at once.
async fn pipe_one(
    browser: tokio::net::TcpStream,
    recipe: &SshRecipe,
    gate: &LinkGate,
    port: u16,
    token: Option<&str>,
) -> anyhow::Result<Answer> {
    let mut child = match gate.spawn_async(recipe) {
        Ok(child) => child,
        Err(SpawnError::LinkDown) => return Ok(Answer::LinkDown),
        Err(e) => return Err(anyhow::Error::new(e).context(format!("spawn ssh {recipe}"))),
    };
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
    // login. The child's last stderr line is the diagnosis when it died
    // before or during the splice — beats a generic broken-pipe message.
    if result.is_err() {
        if let Some(line) = sot_protocol::topology::ssh_bridge::last_stderr_after_failure(&last_stderr).await {
            return Err(anyhow::anyhow!(line));
        }
    }
    result
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

/// Hand `url` (any browser-openable address — `http://…`, `file:///…`, or
/// a local filesystem path) off to the OS default handler. Fire-and-
/// forget — we don't wait for the browser to exit.
pub(crate) fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // Avoid `cmd /c start`: shell metacharacters in URLs, especially
        // `&secret=...` on Pluto links, are otherwise parsed by cmd.exe.
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
            .map(|_| ())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())?;
    }
    tracing::info!(%url, "opened in browser");
    Ok(())
}

/// Write `html_bytes` to a unique temp file and hand it off to the OS
/// default browser via `open_url_in_browser`. We don't delete the temp
/// file (the OS cleans temp on its own schedule; a fresh path per call
/// also prevents the browser from showing a stale cached version).
pub(crate) fn open_html_in_browser(html_bytes: &[u8]) -> std::io::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut path = std::env::temp_dir();
    path.push(format!("sot-preview-{now}.html"));
    std::fs::write(&path, html_bytes)?;
    let path_str = path.to_string_lossy().to_string();
    open_url_in_browser(&path_str)
}

#[cfg(test)]
mod tests {
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
            let answer = super::pipe_one(browser, &recipe, &gate, addr.port(), None).await;
            assert!(matches!(answer, Ok(super::Answer::LinkDown)), "got {answer:?}");
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
        #[derive(Clone)]
        struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Buf(Arc::new(std::sync::Mutex::new(Vec::new())));
        let writer = log.clone();
        // #[tokio::test] runs every task on this thread, so a thread default sees the listener's spawned tasks.
        let _log = tracing::subscriber::set_default(
            tracing_subscriber::fmt().with_ansi(false).with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish(),
        );
        let warns = || String::from_utf8_lossy(&log.0.lock().unwrap()).matches("WARN").count();
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
        let task = tokio::spawn(super::serve_listener(listener, port, arm.clone(), dial));
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
        let task = tokio::spawn(super::serve_listener(listener, port, arm.clone(), dial));
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
}
