//! Lease tests: taking and holding a lease.

use super::*;
use interprocess::local_socket::{GenericFilePath, ListenerOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub(super) fn bind(tag: &str) -> (interprocess::local_socket::tokio::Listener, PathBuf) {
    let unique = format!(
        "sot-lease-test-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    #[cfg(windows)]
    let path = PathBuf::from(format!(r"\\.\pipe\{unique}"));
    #[cfg(not(windows))]
    let path = {
        // macOS's temp_dir() is long enough to overflow sun_path
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let _ = &unique;
        PathBuf::from(format!(
            "/tmp/sl-{tag}-{}-{}.sock",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    };
    let _ = std::fs::remove_file(&path);
    let name = path.to_str().unwrap().to_fs_name::<GenericFilePath>().unwrap();
    let listener = ListenerOptions::new().name(name).create_tokio().expect("bind");
    (listener, path)
}

fn pipe_config(path: &Path) -> TransportConfig {
    TransportConfig { dial: Dial::Pipe(path.to_path_buf()), token: None }
}

#[test]
fn harness_never_leases() {
    for bits in 0..8u8 {
        let (e, c, n) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
        assert_eq!(lease_exempt(e, c, n), bits != 0, "row {bits}");
    }
    let ssh = sot_protocol::ssh_bridge::SshRecipe::new("somehost", None).unwrap();
    let conns = vec![
        ("local".to_string(), pipe_config(Path::new("/x"))),
        ("far".to_string(), TransportConfig { dial: Dial::Ssh(ssh), token: None }),
    ];
    assert_eq!(pipe_hosts(&conns), vec!["local".to_string()]);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (listener, path) = bind("exempt");
        let host = "local".to_string();
        let leases = Leases::new(true, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let accepted = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
        assert!(accepted.is_err(), "an exempt window must not connect");
        assert_eq!(leases.notice(), None);
    });
}

#[tokio::test]
async fn lease_retried_each_data_connection() {
    let (listener, path) = bind("retry");
    let count = Arc::new(AtomicUsize::new(0));
    let (drop_tx, drop_rx) = oneshot::channel::<()>();
    let c = count.clone();
    tokio::spawn(async move {
        let mut drop_rx = Some(drop_rx);
        loop {
            let conn = listener.accept().await.unwrap();
            let n = c.fetch_add(1, Ordering::SeqCst) + 1;
            let (rx, mut tx) = conn.split();
            let mut rx = codec::buffered(rx);
            let (req, _) = codec::read_frame(&mut rx).await.unwrap();
            let payload = match n {
                1 => serde_json::json!({"error": "unknown op: fe.lease"}),
                2 => serde_json::json!({"outcome": "closing"}),
                3 => {
                    // No reply, then the connection ends: a failed connect,
                    // reached at the end, not at the wait.
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(400)).await;
                        drop((rx, tx));
                    });
                    continue;
                }
                4 => serde_json::json!({"outcome": "foreign"}),
                5 => serde_json::json!({"outcome": "undetermined"}),
                _ => serde_json::json!({"outcome": "granted", "state_root": "r"}),
            };
            codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, payload), None)
                .await
                .unwrap();
            if n == 6 {
                let rx_drop = drop_rx.take().unwrap();
                tokio::spawn(async move {
                    let _ = rx_drop.await;
                    drop((rx, tx));
                });
            } else if n > 6 {
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    drop((rx, tx));
                });
            }
        }
    });
    let host = "local".to_string();
    let mut leases = Leases::new(false, vec![host.clone()]);
    Arc::get_mut(&mut leases).unwrap().reply_wait = Duration::from_millis(100);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(leases.standing(&host), Some(Standing::Unsupported));
    // Closing, and a connection that ends unanswered: each a failed connect.
    assert!(leases.before_data_connection(&host, &path, None).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(leases.standing(&host), Some(Standing::Unreached));
    assert!(leases.before_data_connection(&host, &path, None).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 3);
    assert_eq!(leases.standing(&host), Some(Standing::Unreached));
    // Foreign and Undetermined proceed unleased, and the next connection asks again.
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    assert_eq!(count.load(Ordering::SeqCst), 4);
    assert_eq!(leases.standing(&host), Some(Standing::Foreign));
    assert!(!leases.held(&host));
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    assert_eq!(count.load(Ordering::SeqCst), 5);
    assert_eq!(leases.standing(&host), Some(Standing::Undetermined));
    assert!(!leases.held(&host));
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    assert_eq!(count.load(Ordering::SeqCst), 6);
    assert_eq!(leases.standing(&host), Some(Standing::Granted { state_root: Some("r".into()) }));
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    assert_eq!(count.load(Ordering::SeqCst), 6, "a held lease is not retaken");
    drop_tx.send(()).unwrap();
    for _ in 0..40 {
        if !leases.held(&host) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!leases.held(&host), "the dropped stream ends the lease");
    leases.before_data_connection(&host, &path, None).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 7);
}

#[tokio::test]
async fn notice_seen_timeout_keeps_the_lease() {
    let (listener, path) = bind("slowseen");
    let (eof_tx, eof_rx) = oneshot::channel::<bool>();
    tokio::spawn(async move {
        let conn = listener.accept().await.unwrap();
        let (rx, mut tx) = conn.split();
        let mut rx = codec::buffered(rx);
        let (req, _) = codec::read_frame(&mut rx).await.unwrap();
        let granted = serde_json::json!({"outcome": "granted"});
        codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
        let (seen, _) = codec::read_frame(&mut rx).await.unwrap();
        assert_eq!(seen.op, op::FE_NOTICE_SEEN);
        // Never answer; report whether the window's end of the stream closes.
        let eof = tokio::time::timeout(Duration::from_millis(500), codec::read_frame(&mut rx)).await.is_ok();
        let _ = eof_tx.send(eof);
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(tx);
    });
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    leases.notice_seen(&host, 2);
    assert!(!eof_rx.await.unwrap(), "the fake daemon read an EOF after a slow notice_seen reply");
    assert!(leases.held(&host), "a slow notice_seen reply must not end the lease");
}

#[tokio::test]
async fn lease_wire_round_trip() {
    let (listener, path) = bind("wire");
    let (seen_tx, seen_rx) = oneshot::channel::<Frame>();
    let (leaving_tx, leaving_rx) = oneshot::channel::<Frame>();
    tokio::spawn(async move {
        let conn = listener.accept().await.unwrap();
        let (rx, mut tx) = conn.split();
        let mut rx = codec::buffered(rx);
        let (req, _) = codec::read_frame(&mut rx).await.unwrap();
        assert_eq!(req.op, op::FE_LEASE);
        assert_eq!(req.id, 1);
        let me = self_identity().unwrap();
        assert_eq!(req.payload["boot"], serde_json::json!(me.boot));
        assert_eq!(req.payload["pid"], serde_json::json!(me.pid));
        assert_eq!(req.payload["created"], serde_json::json!(me.created));
        assert_eq!(req.payload["token"], serde_json::json!("tok"));
        let granted = serde_json::json!({"outcome": "granted", "state_root": "r", "not_ended": 1});
        codec::write_frame(&mut tx, &Frame::res(1, op::FE_LEASE, granted), None).await.unwrap();
        let (seen, _) = codec::read_frame(&mut rx).await.unwrap();
        codec::write_frame(&mut tx, &Frame::res(seen.id, op::FE_NOTICE_SEEN, serde_json::json!({})), None)
            .await
            .unwrap();
        let _ = seen_tx.send(seen);
        let (leaving, _) = codec::read_frame(&mut rx).await.unwrap();
        let _ = leaving_tx.send(leaving.clone());
        codec::write_frame(&mut tx, &Frame::res(leaving.id, op::FE_LEAVING, serde_json::json!({"not_ended": 3})), None)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, Some("tok")).await.unwrap(), 1);
    assert_eq!(leases.granted_state_roots(), vec!["r".to_string()]);
    leases.notice_seen(&host, 1);
    let seen = tokio::time::timeout(Duration::from_secs(2), seen_rx).await.unwrap().unwrap();
    assert_eq!(seen.op, op::FE_NOTICE_SEEN);
    assert_eq!(seen.payload, serde_json::json!({"not_ended": 1}));
    let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), leaving_rx).await.unwrap().unwrap();
    assert_eq!(frame.op, op::FE_LEAVING);
    assert_eq!(frame.payload, serde_json::json!({"intent": "close"}));
    let mut step = leaving.poll(Instant::now());
    for _ in 0..40 {
        if !matches!(step, LeaveStep::Wait(_)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        step = leaving.poll(Instant::now());
    }
    assert_eq!(step, LeaveStep::Show);
    assert_eq!(leaving.line(), not_ended_line(3));
}

#[tokio::test]
async fn slow_grant_is_held_not_dropped() {
    let (listener, path) = bind("slowgrant");
    let accepts = Arc::new(AtomicUsize::new(0));
    let eof = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (a, e) = (accepts.clone(), eof.clone());
    tokio::spawn(async move {
        loop {
            let conn = listener.accept().await.unwrap();
            a.fetch_add(1, Ordering::SeqCst);
            let e = e.clone();
            tokio::spawn(async move {
                let (rx, mut tx) = conn.split();
                let mut rx = codec::buffered(rx);
                let (req, _) = codec::read_frame(&mut rx).await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                let granted = serde_json::json!({"outcome": "granted", "state_root": "r", "not_ended": 2});
                codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
                if tokio::time::timeout(Duration::from_millis(500), codec::read_frame(&mut rx)).await.is_ok() {
                    e.store(true, Ordering::SeqCst);
                }
            });
        }
    });
    let host = "local".to_string();
    let mut leases = Leases::new(false, vec![host.clone()]);
    Arc::get_mut(&mut leases).unwrap().reply_wait = Duration::from_millis(100);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 2);
    assert!(leases.held(&host));
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!eof.load(Ordering::SeqCst), "a granted lease's connection was dropped");
}

#[tokio::test]
async fn one_daemon_under_two_labels_is_one_count() {
    // A fake daemon that grants every connection `root` and `n`, logging each later frame.
    fn daemon(
        listener: interprocess::local_socket::tokio::Listener,
        root: &'static str,
        n: u32,
    ) -> (Arc<std::sync::Mutex<Vec<String>>>, Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>) {
        let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let conns = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (l, c) = (log.clone(), conns.clone());
        tokio::spawn(async move {
            loop {
                let conn = listener.accept().await.unwrap();
                let l = l.clone();
                let task = tokio::spawn(async move {
                    let (rx, mut tx) = conn.split();
                    let mut rx = codec::buffered(rx);
                    let (req, _) = codec::read_frame(&mut rx).await.unwrap();
                    let granted = serde_json::json!({"outcome": "granted", "state_root": root, "not_ended": n});
                    codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
                    while let Ok((f, _)) = codec::read_frame(&mut rx).await {
                        l.lock().unwrap().push(f.op.to_string());
                    }
                });
                c.lock().unwrap().push(task.abort_handle());
            }
        });
        (log, conns)
    }
    let (l1, p1) = bind("twolabels1");
    let (l2, p2) = bind("twolabels2");
    let (l3, p3) = bind("twolabels3");
    let (log1, conns1) = daemon(l1, "r", 3);
    let (_log2, _conns2) = daemon(l2, "s", 4);
    let (_log3, _conns3) = daemon(l3, "t", 0);
    let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
    let leases = Leases::new(false, vec![a.clone(), b.clone(), c.clone()]);
    assert_eq!(leases.before_data_connection(&a, &p1, None).await.unwrap(), 3);
    assert_eq!(leases.before_data_connection(&b, &p1, None).await.unwrap(), 3);
    assert_eq!(leases.before_data_connection(&c, &p2, None).await.unwrap(), 4);
    let rs = vec![("r".to_string(), 3), ("s".to_string(), 4)];
    assert_eq!(leases.owed(), rs, "a count is owed per label, not per granting daemon");
    // a re-leases a replacement daemon (nothing not ended) before any frame drew the counts.
    conns1.lock().unwrap()[0].abort();
    for _ in 0..40 {
        if !leases.held(&a) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!leases.held(&a));
    assert_eq!(leases.before_data_connection(&a, &p3, None).await.unwrap(), 0);
    assert_eq!(leases.owed(), rs, "a re-lease moved or dropped a count another daemon granted");
    leases.notice_seen(&"s".to_string(), 9);
    assert_eq!(leases.owed(), rs, "an ack of another count cleared the owed one");
    leases.notice_seen(&"r".to_string(), 3);
    for _ in 0..40 {
        if log1.lock().unwrap().iter().any(|o| o == op::FE_NOTICE_SEEN) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        log1.lock().unwrap().iter().any(|o| o == op::FE_NOTICE_SEEN),
        "the ack did not reach the daemon that granted the count: {:?}",
        log1.lock().unwrap()
    );
    assert_eq!(leases.owed(), vec![("s".to_string(), 4)], "a shown count is still owed");
}

// PIN, NOT FAIL-FIRST: interprocess 2.4.2 already creates the socket
// SOCK_CLOEXEC (os/unix/c_wrappers.rs:177), so this passed on its first
// run. On Windows the pipe client handle is non-inheritable because
// CreateFileW is given null security attributes
// (os/windows/named_pipe/c_wrappers.rs:158-165); there is no Windows half.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn lease_stream_not_inherited() {
    fn sockets() -> std::collections::HashSet<String> {
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| p.starts_with("socket:"))
            .collect()
    }
    let (_listener, path) = bind("cloexec");
    let before = sockets();
    let _stream = connect_pipe(&path).await.unwrap();
    let ours: Vec<String> = sockets().difference(&before).cloned().collect();
    assert!(!ours.is_empty(), "the connect must have opened a socket");
    let out = std::process::Command::new("sh")
        .args(["-c", "for f in /proc/self/fd/*; do readlink $f; done"])
        .output()
        .unwrap();
    let seen = String::from_utf8_lossy(&out.stdout);
    for s in &ours {
        assert!(!seen.contains(s.as_str()), "{s} leaked into a child");
    }
}

#[test]
fn no_lease_status_line() {
    use Standing::*;
    let granted = || Granted { state_root: None };
    assert_eq!(lease_notice(true, &[Foreign]), None);
    assert_eq!(lease_notice(true, &[]), None);
    assert_eq!(lease_notice(false, &[granted()]), None);
    assert_eq!(lease_notice(false, &[granted(), Foreign]), None);
    assert_eq!(lease_notice(false, &[Pending]), None);
    assert_eq!(lease_notice(false, &[Undetermined]), Some(NOTICE_UNDETERMINED));
    assert_eq!(lease_notice(false, &[Unsupported]), Some(NOTICE_UNSUPPORTED));
    assert_eq!(lease_notice(false, &[Foreign]), Some(NOTICE_NO_BACKEND));
    assert_eq!(lease_notice(false, &[Unreached]), Some(NOTICE_NO_BACKEND));
    assert_eq!(lease_notice(false, &[]), Some(NOTICE_NO_BACKEND));
    assert_eq!(lease_notice(false, &[Undetermined, Unsupported]), Some(NOTICE_UNDETERMINED));
    for set in [&[Undetermined][..], &[Unsupported], &[Foreign], &[Unreached], &[]] {
        let notice = lease_notice(false, set).expect("a no-lease set has a notice");
        assert!(notice.starts_with("closing will not end sessions"), "{notice}");
    }
    // A granted slot whose holder has ended reads as unreached.
    let (tx, rx) = mpsc::unbounded_channel::<HolderCmd>();
    drop(rx);
    let leases = Leases::new(false, vec!["h".to_string()]);
    leases.set(&"h".to_string(), granted(), Some(tx));
    assert_eq!(leases.standing(&"h".to_string()), Some(Unreached));
    assert_eq!(leases.notice(), Some(NOTICE_NO_BACKEND));
}
