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
    tokio::spawn(sot_platform::identity::peer_owner::serve_own(listener, "page-proxy", move |browser| {
        let arm = std::sync::Arc::clone(&arm);
        let done = dial(browser);
        async move { super::serve_browser(port, &arm, done).await }
    }))
}

/// A daemon that falls silent in the proxy handshake ends it at the 30 s connect bound, naming the step: before
/// the hello's reply, and before proxy.connect's. The browser pair is made first; then the clock is paused, so the
/// bound runs at its real 30 s.
#[tokio::test]
async fn a_silent_daemon_ends_the_proxy_handshake_naming_the_step() {
    use sot_protocol::{codec, op, Frame};
    use std::time::Duration;
    use tokio::io::BufReader;
    for (answered, step) in [(0, "hello"), (1, "proxy.connect")] {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (_client, accepted) =
            tokio::join!(tokio::net::TcpStream::connect(addr), listener.accept());
        let browser = accepted.unwrap().0;
        let (fe, daemon) = tokio::io::duplex(4096);
        let held = tokio::spawn(async move {
            let (d_rd, mut d_wr) = tokio::io::split(daemon);
            let mut d_buf = BufReader::new(d_rd);
            if answered > 0 {
                let (hello, _) = codec::read_frame(&mut d_buf).await.unwrap();
                let accepted = serde_json::json!({ "session_id": "s", "revision": 0, "snapshot_pending": false });
                codec::write_frame(&mut d_wr, &Frame::res(hello.id, op::HELLO, accepted), None)
                    .await
                    .unwrap();
            }
            std::future::pending::<()>().await;
        });
        let (f_rd, mut f_wr) = tokio::io::split(fe);
        let mut f_buf = BufReader::new(f_rd);
        tokio::time::pause();
        let started = tokio::time::Instant::now();
        let handshake = super::pipe_one_over(&mut f_wr, &mut f_buf, browser, 1, None);
        let ended = tokio::time::timeout(Duration::from_secs(60), handshake)
            .await
            .unwrap_or_else(|_| {
                panic!("silent before the {step} reply: still waiting after 60s")
            });
        let at = started.elapsed();
        tokio::time::resume();
        held.abort();
        let err = format!(
            "{:#}",
            ended
                .err()
                .unwrap_or_else(|| panic!("{step}: a silent daemon was answered"))
        );
        assert_eq!(err, format!("{step} reply timed out after 30s"));
        assert!(
            at >= Duration::from_secs(30) && at < Duration::from_secs(31),
            "{step}: ended after {at:?}"
        );
        println!("proxy-handshake: silent before the {step} reply -> \"{err}\" after {at:?}");
    }
}

/// fe-net: with the host's link down a browser connection closes at once with `Answer::LinkDown`, and no channel is
/// opened: a handle that was never up, and one whose session put it down, the router seeing no connection for either.
#[tokio::test]
async fn a_down_link_closes_a_browser_connection_at_once() {
    use tokio::io::AsyncReadExt;
    let made = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (_link, put_down) = crate::net::link_fixture::link_to(std::path::PathBuf::from("/nonexistent/d.sock"), made.clone()).await;
    put_down.set_down();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    for handle in [sot_topology::link::LinkHandle::default(), put_down] {
        for _ in 0..3 {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (browser, _) = listener.accept().await.unwrap();
            let t0 = std::time::Instant::now();
            let answer = super::pipe_one(browser, &handle, addr.port(), None).await;
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
    assert_eq!(made.load(std::sync::atomic::Ordering::SeqCst), 0, "a down link opened no channel");
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
    let log = sot_platform::test_log::capture();
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

/// A private folder and socket path for a stand-in relay, so no other account on a shared host reaches the socket.
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

/// A page dial opens a channel on the host's link and reaches the stand-in daemon with the handoff hello and
/// `proxy.connect`, and the bytes written right behind the daemon's reply reach the browser whole; a `bad_port` answer
/// is the usual refusal that parks the port.
#[cfg(unix)]
#[tokio::test]
async fn a_page_channel_splices_after_handoff_and_reports_bad_port() {
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
        let made = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (link, handle) = crate::net::link_fixture::link_to(path.clone(), made.clone()).await;
        let (browser, mut client) = browser_pair().await;
        let dial = tokio::spawn(async move { super::pipe_one(browser, &handle, 7000, None).await });
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
        assert_eq!(made.load(std::sync::atomic::Ordering::SeqCst), 1, "one browser connection, one channel");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), daemon).await;
        drop(link);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

/// A page handshake in flight when the host's link ends ends with it: the browser connection closes as on a down link
/// (`LinkDown`), not at the 30 s reply bound. The stand-in daemon takes the channel's connection and answers nothing.
#[cfg(unix)]
#[tokio::test]
async fn a_page_handshake_in_flight_ends_when_the_link_ends() {
    use std::time::Duration;
    let path = relay_stand_in_path("close");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let made = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (link, handle) = crate::net::link_fixture::link_to(path.clone(), made).await;
    let (browser, _client) = browser_pair().await;
    let dial = tokio::spawn(async move {
        let answer = super::pipe_one(browser, &handle, 7000, None).await;
        (answer, std::time::Instant::now())
    });
    let _held = tokio::time::timeout(Duration::from_secs(30), listener.accept()).await.expect("the channel's connection").unwrap();
    let ended = std::time::Instant::now();
    drop(link);
    let (answer, at) = tokio::time::timeout(Duration::from_secs(40), dial).await.expect("the page dial ended").unwrap();
    let after = at.duration_since(ended);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    println!("page handshake in flight at a link's end: answer={answer:?} after_end_ms={}", after.as_millis());
    assert!(
        matches!(answer, Ok(super::Answer::LinkDown)) && after < Duration::from_secs(5),
        "the page handshake outlived its link: {answer:?}, {after:?} after the end"
    );
}

/// The link's end ends an established page whatever its copies wait on: a browser that stopped reading, so the copy to
/// it blocks in its write and the copy from it waits for bytes that never come, has its connection closed and its
/// task ended within 5 s of the link's end. The stand-in daemon answers the handoff, then writes without end.
#[cfg(unix)]
#[tokio::test]
async fn a_link_end_closes_a_page_whose_browser_stopped_reading() {
    use sot_protocol::{codec, op, Frame};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let path = relay_stand_in_path("stall");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let daemon = tokio::spawn(async move {
        let (conn, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = conn.into_split();
        let mut rd = tokio::io::BufReader::new(rd);
        let (hello, _) = codec::read_frame(&mut rd).await.unwrap();
        let (req, _) = codec::read_frame(&mut rd).await.unwrap();
        let accepted = serde_json::json!({ "session_id": "s", "revision": 0, "snapshot_pending": false });
        codec::write_frame(&mut wr, &Frame::res(hello.id, op::HELLO, accepted), None).await.unwrap();
        codec::write_frame(&mut wr, &Frame::res(req.id, op::PROXY_CONNECT, serde_json::json!({ "ok": true })), None).await.unwrap();
        let page = vec![b'p'; 64 * 1024];
        while wr.write_all(&page).await.is_ok() {}
    });
    let made = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (link, handle) = crate::net::link_fixture::link_to(path.clone(), made).await;
    let (browser, mut client) = browser_pair().await;
    let dial = tokio::spawn(async move { super::pipe_one(browser, &handle, 7000, None).await });
    // Long enough for the page to fill the browser's socket and the channel's window behind it.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!dial.is_finished(), "the page is established and stalled");
    drop(link);
    let answer = tokio::time::timeout(Duration::from_secs(5), dial).await.expect("the page's task ends with its link").unwrap();
    assert!(matches!(answer, Ok(super::Answer::LinkDown)), "got {answer:?}");
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut rest)).await.expect("the browser's connection closes").ok();
    daemon.abort();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A page's open is bounded: a router that keeps its link up but never answers the open ends the page with
/// `OpenError::TimedOut` at the bound, and the answer that comes late opens nothing: the window closes that channel.
#[cfg(unix)]
#[tokio::test]
async fn a_page_open_the_router_never_answers_times_out_and_its_late_answer_is_closed() {
    use sot_protocol::{codec, op, Frame};
    use sot_topology::link::{Link, LinkHandle, OpenError};
    use std::time::{Duration, Instant};
    let (near, far) = tokio::io::duplex(1 << 20);
    let (prx, mut ptx) = tokio::io::split(far);
    let mut prx = tokio::io::BufReader::new(prx);
    let (wrx, wtx) = tokio::io::split(near);
    let start = tokio::spawn(Link::over(wrx, wtx));
    let (hello, _) = codec::read_frame(&mut prx).await.unwrap();
    let answer = serde_json::json!({ "link": sot_protocol::ops::link::LINK, "build": "stand-in" });
    codec::write_frame(&mut ptx, &Frame::res(hello.id, op::LINK_HELLO, answer), None).await.unwrap();
    let link = start.await.unwrap().expect("the link starts");
    let handle = LinkHandle::default();
    handle.set_up(&link);
    let (browser, _client) = browser_pair().await;
    let started = Instant::now();
    let bound = Duration::from_millis(300);
    let dial = tokio::spawn(async move { super::pipe_one_within(browser, &handle, 7000, None, bound).await });
    let (open, _) = tokio::time::timeout(Duration::from_secs(5), codec::read_frame(&mut prx)).await.expect("the page's open").unwrap();
    assert_eq!(open.op, op::LINK_OPEN);
    let answer = tokio::time::timeout(Duration::from_secs(5), dial).await.expect("the page ends at its open's bound").unwrap();
    let after = started.elapsed();
    let error = answer.expect_err("an unanswered open is the page's error");
    assert_eq!(error.downcast_ref::<OpenError>(), Some(&OpenError::TimedOut), "{error:#}");
    assert!(after >= bound && after < bound + Duration::from_secs(2), "the open ended {after:?} after it began");
    codec::write_frame(&mut ptx, &Frame::res(open.id, op::LINK_OPEN, serde_json::json!({ "ch": open.payload["ch"], "ok": true })), None).await.unwrap();
    let (close, _) = tokio::time::timeout(Duration::from_secs(5), codec::read_frame(&mut prx)).await.expect("the late channel's close").unwrap();
    assert_eq!((close.op.as_str(), &close.payload["ch"], close.payload["reason"].as_str()), (op::LINK_CLOSE, &open.payload["ch"], Some("reset")));
    drop(link);
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
/// PAGE-PORT: the window's listener and the daemon's page port are different numbers on different computers. A
/// browser connection to the armed listener, through the manager as production runs it, reaches the daemon as
/// `proxy.connect` naming the daemon's port, never the listener's own.
#[cfg(unix)]
#[test]
fn the_manager_dials_the_daemons_port_not_the_listeners() {
    use sot_protocol::{codec, op, Frame};
    use tokio::io::AsyncWriteExt;
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let path = relay_stand_in_path("port");
    let daemon_port = 7000;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    super::spawn_proxy_manager(&rt, rx);
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let local = listener.local_addr().unwrap().port();
    assert_ne!(local, daemon_port);
    let asked = rt.block_on(async {
        let relay = tokio::net::UnixListener::bind(&path).unwrap();
        let made = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (link, handle) = crate::net::link_fixture::link_to(path.clone(), made).await;
        tx.send(super::PageListener {
            listener,
            daemon_port,
            link: handle,
            token: None,
            arm: std::sync::Arc::new(super::Arm::default()),
        })
        .unwrap();
        let _link = link;
        let _browser = tokio::net::TcpStream::connect(("127.0.0.1", local)).await.unwrap();
        let (conn, _) = tokio::time::timeout(std::time::Duration::from_secs(5), relay.accept())
            .await
            .expect("the browser connection must reach the daemon over a channel")
            .unwrap();
        let (rd, mut wr) = conn.into_split();
        let mut rd = tokio::io::BufReader::new(rd);
        let bound = std::time::Duration::from_secs(30);
        let (hello, _) = tokio::time::timeout(bound, codec::read_frame(&mut rd))
            .await
            .expect("the relay's hello within 30 s")
            .unwrap();
        let (req, _) = tokio::time::timeout(bound, codec::read_frame(&mut rd))
            .await
            .expect("the relay's proxy.connect within 30 s")
            .unwrap();
        assert_eq!(req.op, op::PROXY_CONNECT);
        let mut out = Vec::new();
        let accepted = serde_json::json!({ "session_id": "s", "revision": 0, "snapshot_pending": false });
        codec::write_frame(&mut out, &Frame::res(hello.id, op::HELLO, accepted), None).await.unwrap();
        let refused = serde_json::json!({ "error": "not proxyable", "code": "bad_port" });
        codec::write_frame(&mut out, &Frame::res(req.id, op::PROXY_CONNECT, refused), None).await.unwrap();
        wr.write_all(&out).await.unwrap();
        req.payload["port"].as_u64()
    });
    assert_eq!(asked, Some(u64::from(daemon_port)));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
