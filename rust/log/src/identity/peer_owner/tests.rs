use super::*;

// Captured 2026-10-03 on a Linux box with the brief's Python block: a client connected to a listener on
// 127.0.0.1, both ends in this process's account, the header and three of its loopback rows. The last
// two rows (client 33004 -> server 58247, then the reverse) are that connection; the first is another
// account's loopback pair, kept to show a row that is not ours.
const CAPTURE: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
  31: 0100007F:A8B2 0100007F:9931 01 00000000:00000000 00:00000000 00000000  1028        0 2285304547 1 0000000000000000 20 4 0 10 -1
  47: 0100007F:80EC 0100007F:E387 01 00000000:00000000 00:00000000 00000000  1001        0 2335972813 2 0000000000000000 20 0 0 10 -1
 240: 0100007F:E387 0100007F:80EC 01 00000000:00000000 00:00000000 00000000  1001        0 2335972814 1 0000000000000000 20 0 0 10 -1
";

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

#[test]
fn the_tcp_table_parser_reads_a_captured_loopback_table() {
    let (client, server) = (loopback(0x80EC), loopback(0xE387));
    assert_eq!(uid_of_row(CAPTURE, client, server), Some(1001));
    assert_eq!(uid_of_row(CAPTURE, server, client), Some(1001));
    assert_eq!(
        uid_of_row(CAPTURE, loopback(0xA8B2), loopback(0x9931)),
        Some(1028)
    );
    assert_eq!(uid_of_row(CAPTURE, client, loopback(1)), None);
    assert_eq!(parse_endpoint("0100007F:1F90"), Some(loopback(8080)));
    assert_eq!(parse_endpoint("0100007F"), None);
    assert_eq!(
        verdict("uid:0", "uid:1001"),
        PeerOwner::Other("uid:0".into())
    );
    assert_eq!(verdict("uid:1001", "uid:1001"), PeerOwner::Mine);
}

#[test]
fn a_closing_or_ownerless_row_names_no_owner() {
    let (client, server) = (loopback(0x80EC), loopback(0xE387));
    let header = CAPTURE.lines().next().unwrap();
    let row = CAPTURE
        .lines()
        .find(|l| l.contains("0100007F:80EC 0100007F:E387"))
        .unwrap();
    // A FIN_WAIT2/TIME_WAIT orphan: state 06, uid 0, inode 0.
    let closing = row
        .replace(" 01 ", " 06 ")
        .replace("1001        0 2335972813", "   0        0 0");
    // A live state with no inode: uid 0, inode 0.
    let ownerless = row.replace("1001        0 2335972813", "   0        0 0");
    assert_ne!(closing, row);
    assert_ne!(ownerless, row);
    for orphan in [closing, ownerless] {
        let table = format!("{header}\n{orphan}\n");
        assert_eq!(uid_of_row(&table, client, server), None, "{orphan}");
    }
    assert_eq!(uid_of_row(CAPTURE, client, server), Some(1001));
}

/// A connected pair on loopback: the accepted stream's own address and its peer's.
fn accepted_pair() -> (std::net::TcpStream, std::net::TcpStream) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (accepted, _) = listener.accept().unwrap();
    (accepted, client)
}

#[test]
fn a_connection_from_this_process_is_mine() {
    let (accepted, _client) = accepted_pair();
    let (local, peer) = (
        accepted.local_addr().unwrap(),
        accepted.peer_addr().unwrap(),
    );
    assert_eq!(tcp_peer_owner(local, peer), PeerOwner::Mine);
    assert!(admit("test", local, peer));
}

/// A process started while a connection is being closed can inherit the closed socket's handle on Windows, which
/// keeps the connection open at the other end until that process ends. The tests that start processes (the child
/// tests) and the one that waits for a refused connection to close therefore never run at the same time.
static CHILD_PROCESSES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A test that cannot run on this host says so and passes, except on CI: a hosted runner that silently stops
/// running a test is a failure to be seen, not a pass.
fn skip(reason: &str) {
    eprintln!("skipped: {reason}");
    assert!(
        std::env::var_os("GITHUB_ACTIONS").is_none(),
        "a test skipped on CI: {reason}"
    );
}

/// ADR 0049, User isolation: a client on a dual-stack socket (Java's default, among others) reaches a 127.0.0.1
/// listener with an IPv4-mapped address, so its row is in the IPv6 table (Linux `tcp6`) and the verdict must still
/// name its owner. Not on Windows, whose sockets are IPv6-only by default (its IPv6 path has the next test).
#[cfg(not(windows))]
#[test]
fn a_dual_stack_client_of_an_ipv4_listener_is_mine() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let Ok(_client) =
        std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped(), port))
    else {
        return skip("no dual-stack connect on this host");
    };
    let (accepted, _) = listener.accept().unwrap();
    let (local, peer) = (
        accepted.local_addr().unwrap(),
        accepted.peer_addr().unwrap(),
    );
    assert_eq!(
        tcp_peer_owner(local, peer),
        PeerOwner::Mine,
        "{local} <- {peer}"
    );
}

/// ADR 0049, User isolation: both ends on `[::1]`, so the row is in the IPv6 table on every platform (Linux `tcp6`,
/// macOS's IPv6 records, Windows' `MIB_TCP6ROW_OWNER_MODULE` rows) and the verdict names its owner.
#[test]
fn a_loopback_ipv6_client_of_an_ipv6_listener_is_mine() {
    let Ok(listener) = std::net::TcpListener::bind("[::1]:0") else {
        return skip("no IPv6 loopback on this host");
    };
    let _client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (accepted, _) = listener.accept().unwrap();
    let (local, peer) = (
        accepted.local_addr().unwrap(),
        accepted.peer_addr().unwrap(),
    );
    assert!(local.is_ipv6() && peer.is_ipv6(), "{local} <- {peer}");
    assert_eq!(
        tcp_peer_owner(local, peer),
        PeerOwner::Mine,
        "{local} <- {peer}"
    );
}

#[test]
fn a_peer_no_socket_has_is_refused() {
    // A free port Q and a peer no socket holds: no row, so refused.
    let q = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    assert_ne!(tcp_peer_owner(loopback(q), loopback(1)), PeerOwner::Mine);
    assert!(!admit("test", loopback(q), loopback(1)));
}

/// ADR 0049, User isolation: a connection the owner check refuses reaches no handler, though the handler would
/// have answered.
#[tokio::test]
async fn a_connection_the_owner_check_refuses_reaches_no_handler() {
    let _no_child_process_meanwhile = CHILD_PROCESSES.lock().unwrap_or_else(|p| p.into_inner());
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_in = Arc::clone(&ran);
    let task = tokio::spawn(serve(
        listener,
        "test",
        |name, local, _| {
            owner_decision(
                name,
                local,
                PeerOwner::Unknown("controlled lookup failure".into()),
            )
        },
        move |mut s| {
            let ran = Arc::clone(&ran_in);
            async move {
                ran.fetch_add(1, SeqCst);
                let _ = s.write_all(b"served").await;
            }
        },
    ));
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut raw = Vec::new();
    // A reset counts as the end.
    let _ = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut raw))
        .await
        .expect("the refused connection must close");
    assert!(raw.is_empty(), "got: {}", String::from_utf8_lossy(&raw));
    assert_eq!(
        ran.load(SeqCst),
        0,
        "a refused connection must never reach the handler"
    );
    task.abort();
}

/// ADR 0049, User isolation: controlled ownership observations pass through the production decision.
#[tokio::test]
async fn foreign_or_unknown_owner_is_refused_before_handler() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    static OBSERVED: AtomicUsize = AtomicUsize::new(0);
    fn foreign(name: &'static str, local: SocketAddr, _: SocketAddr) -> bool {
        OBSERVED.fetch_add(1, SeqCst);
        owner_decision(
            name,
            local,
            PeerOwner::Other("controlled foreign owner".into()),
        )
    }
    fn unknown(name: &'static str, local: SocketAddr, _: SocketAddr) -> bool {
        OBSERVED.fetch_add(1, SeqCst);
        owner_decision(
            name,
            local,
            PeerOwner::Unknown("controlled missing owner".into()),
        )
    }
    fn mine(name: &'static str, local: SocketAddr, _: SocketAddr) -> bool {
        OBSERVED.fetch_add(1, SeqCst);
        owner_decision(name, local, PeerOwner::Mine)
    }
    for (check, refused) in [
        (foreign as Admit, true),
        (unknown as Admit, true),
        (mine as Admit, false),
    ] {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ran);
        let before = OBSERVED.load(SeqCst);
        let task = tokio::spawn(serve(listener, "controlled", check, move |mut stream| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, SeqCst);
                stream.write_all(b"served").await.unwrap();
            }
        }));
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut bytes = Vec::new();
        let result =
            tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut bytes)).await;
        task.abort();
        let _ = task.await;
        assert!(
            result.is_ok(),
            "admission did not close or serve the connection"
        );
        assert_eq!(
            OBSERVED.load(SeqCst),
            before + 1,
            "ownership observation was not reached"
        );
        if refused {
            assert_eq!(ran.load(SeqCst), 0, "refused owner reached handler");
            assert!(bytes.is_empty(), "refused owner received reply bytes");
            eprintln!("admission-proof test=identity::peer_owner::tests::foreign_or_unknown_owner_is_refused_before_handler endpoint=page boundary=owner-query fixture=controlled-owner-result rejected=true dispatched=0 bodies=1");
        } else {
            assert_eq!(bytes, b"served");
            assert_eq!(ran.load(SeqCst), 1);
        }
    }
}

/// ADR 0049, User isolation: a flood of connections to one page port runs at most `MAX_LOOKUPS` owner lookups at a
/// time, and every connection is still served in the end.
#[tokio::test]
async fn a_flood_of_connections_runs_a_bounded_number_of_lookups() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use tokio::io::AsyncReadExt;
    static RUNNING: AtomicUsize = AtomicUsize::new(0);
    static MOST: AtomicUsize = AtomicUsize::new(0);
    fn slow(_: &'static str, _: SocketAddr, _: SocketAddr) -> bool {
        let now = RUNNING.fetch_add(1, SeqCst) + 1;
        MOST.fetch_max(now, SeqCst);
        std::thread::sleep(Duration::from_millis(40));
        RUNNING.fetch_sub(1, SeqCst);
        true
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(serve(listener, "test", slow, |mut s| async move {
        let _ = tokio::io::AsyncWriteExt::write_all(&mut s, b"k").await;
    }));
    let clients: Vec<_> = (0..MAX_LOOKUPS * 3)
        .map(|_| {
            tokio::spawn(async move {
                let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
                let mut b = [0u8; 1];
                tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut b))
                    .await
                    .unwrap()
                    .unwrap();
            })
        })
        .collect();
    for c in clients {
        c.await.unwrap();
    }
    assert!(
        MOST.load(SeqCst) <= MAX_LOOKUPS,
        "{} lookups ran at once",
        MOST.load(SeqCst)
    );
    assert!(
        MOST.load(SeqCst) > 1,
        "the lookups did not overlap, so the bound was not exercised"
    );
    task.abort();
}

/// ADR 0049, User isolation: the connections beyond the lookup bound wait in the kernel's backlog, which is
/// bounded, not as accepted streams each holding a descriptor of the daemon (a flood must not reach EMFILE). Linux,
/// measured on the property itself: the server-side rows of `/proc/net/tcp` on the listener's port, ESTABLISHED
/// with an inode, are the accepted streams (a connection still in the backlog has inode 0), whatever else this
/// process is doing meanwhile.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_flood_waits_in_the_backlog_not_in_open_streams() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    static RELEASE: AtomicBool = AtomicBool::new(false);
    fn held(_: &'static str, _: SocketAddr, _: SocketAddr) -> bool {
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while !RELEASE.load(SeqCst) && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }
    fn accepted_streams(port: u16) -> usize {
        let table = std::fs::read_to_string("/proc/net/tcp").unwrap();
        table
            .lines()
            .skip(1)
            .filter_map(|line| {
                let f: Vec<&str> = line.split_whitespace().collect();
                let local = parse_endpoint(f.get(1)?)?;
                (local.port() == port && *f.get(3)? == "01" && *f.get(9)? != "0").then_some(())
            })
            .count()
    }
    const FLOOD: usize = 64;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(serve(listener, "test", held, |_s| async {}));
    let mut clients = Vec::new();
    for _ in 0..FLOOD {
        clients.push(tokio::net::TcpStream::connect(addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let accepted = accepted_streams(addr.port());
    RELEASE.store(true, SeqCst);
    task.abort();
    assert!(
        accepted > 0,
        "no accepted stream seen: the measurement did not see the server's rows"
    );
    assert!(
        accepted <= MAX_LOOKUPS,
        "{accepted} accepted streams for {FLOOD} clients and {MAX_LOOKUPS} lookups"
    );
}

/// ADR 0049, User isolation: this account's connection is served through the real check.
#[tokio::test]
async fn this_accounts_connection_reaches_the_handler() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(serve_own(listener, "test", |mut s| async move {
        let _ = s.write_all(b"served").await;
    }));
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut raw))
        .await
        .expect("this account's connection must be served")
        .unwrap();
    assert_eq!(raw, b"served");
    task.abort();
}

#[cfg(windows)]
#[test]
fn this_process_is_mine_and_the_system_process_is_not() {
    let own = crate::identity::os_account::own_account_id().unwrap();
    assert_eq!(
        imp::pid_owner(std::process::id(), i64::MAX, &own),
        PeerOwner::Mine
    );
    // Pid 4 is the System process: its token is another account's, or it cannot be opened at all.
    assert_ne!(imp::pid_owner(4, i64::MAX, &own), PeerOwner::Mine);
}

#[cfg(windows)]
#[test]
fn a_connection_whose_owner_changed_is_refused() {
    let own = crate::identity::os_account::own_account_id().unwrap();
    let binder = imp::Binder {
        pid: std::process::id(),
        bound: i64::MAX,
    };
    assert_eq!(
        imp::owner_of(binder, &own, || Ok(Some(binder))),
        PeerOwner::Mine
    );
    assert_ne!(imp::owner_of(binder, &own, || Ok(None)), PeerOwner::Mine);
    let other_pid = imp::Binder { pid: 4, ..binder };
    assert_ne!(
        imp::owner_of(binder, &own, || Ok(Some(other_pid))),
        PeerOwner::Mine
    );
    let other_bind = imp::Binder {
        bound: i64::MAX - 1,
        ..binder
    };
    assert_ne!(
        imp::owner_of(binder, &own, || Ok(Some(other_bind))),
        PeerOwner::Mine
    );
}

/// ADR 0049, User isolation: the table names the process that bound a socket, and Windows may give that pid to a
/// later process. A process created after the bind is a recycled id, never the connection's owner, even when its
/// token is ours; one created before it is.
#[cfg(windows)]
#[test]
fn a_process_created_after_the_bind_is_refused() {
    let own = crate::identity::os_account::own_account_id().unwrap();
    let me = std::process::id();
    assert!(
        matches!(imp::pid_owner(me, 0, &own), PeerOwner::Unknown(ref why) if why.contains("created after the bind")),
        "this process began after tick 0"
    );
    assert_eq!(imp::pid_owner(me, i64::MAX, &own), PeerOwner::Mine);
}

/// The child side of the child-process tests: connect to the address the parent names, then hold the connection
/// until the parent closes our stdin. A plain run of this test (no variable) does nothing.
#[cfg(windows)]
#[test]
fn child_client_helper() {
    use std::io::Read;
    let Ok(addr) = std::env::var("SOT_PEER_OWNER_CHILD_ADDR") else {
        return;
    };
    let _held = std::net::TcpStream::connect(addr.parse::<SocketAddr>().unwrap()).unwrap();
    println!("connected");
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}

/// ADR 0049, User isolation: a child process started while a connection is being refused does not keep that
/// connection open. The check starts a child that lives about 5 s and refuses; the client must see the connection
/// end within 2 s. The listener is a plain tokio one, the shape the daemon's page listeners had.
#[cfg(windows)]
#[tokio::test]
async fn a_child_started_during_a_refusal_does_not_hold_the_connection() {
    use std::process::{Child, Command, Stdio};
    use tokio::io::AsyncReadExt;
    static CHILD: std::sync::Mutex<Option<Child>> = std::sync::Mutex::new(None);
    fn refuse_and_start_a_child(_: &'static str, _: SocketAddr, _: SocketAddr) -> bool {
        let child = Command::new("ping")
            .args(["-n", "6", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        *CHILD.lock().unwrap() = Some(child);
        false
    }
    let _no_other_child_meanwhile = CHILD_PROCESSES.lock().unwrap_or_else(|p| p.into_inner());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(serve(
        listener,
        "test",
        refuse_and_start_a_child,
        |_s| async {},
    ));
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut raw = Vec::new();
    let closed =
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut raw)).await;
    task.abort();
    if let Some(mut child) = CHILD.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(
        closed.is_ok(),
        "a child started during the refusal held the connection open"
    );
    assert!(raw.is_empty());
}

/// This test binary run as a child that connects to `listener` and holds the connection: the child, the accepted
/// stream and the stdin that ends it.
#[cfg(windows)]
fn child_connecting_to(
    listener: &std::net::TcpListener,
) -> (std::process::Child, std::net::TcpStream) {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "identity::peer_owner::tests::child_client_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            "SOT_PEER_OWNER_CHILD_ADDR",
            listener.local_addr().unwrap().to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The child fails or hangs instead of connecting: fail here after 30 s, not at the CI timeout.
    listener.set_nonblocking(true).unwrap();
    let end = std::time::Instant::now() + Duration::from_secs(30);
    let accepted = loop {
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < end =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                panic!("the child never connected: {e}");
            }
        }
    };
    accepted.set_nonblocking(false).unwrap();
    // The child's own line saying it is connected, within 30 s: a child that cannot say so fails the test here,
    // with everything it did say.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let (said, heard) = std::sync::mpsc::channel();
    {
        let seen = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                seen.lock().unwrap().push_str(&format!("out: {line}\n"));
                // libtest has already printed "test <name> ... " on this line, so the child's word ends it.
                if line.trim().ends_with("connected") {
                    let _ = said.send(());
                }
            }
        });
    }
    {
        let seen = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                seen.lock().unwrap().push_str(&format!("err: {line}\n"));
            }
        });
    }
    if heard.recv_timeout(Duration::from_secs(30)).is_err() {
        let _ = child.kill();
        panic!(
            "the child connected but never said so; it said:\n{}",
            seen.lock().unwrap()
        );
    }
    (child, accepted)
}

/// The Windows orientation, attribution and unit checks the one-process tests cannot make, for a listener on
/// `bind`: the row that matches a connection is the peer's own row, so its binder is the child that connected, not
/// this process; the bind happened after that child began; a process started after the bind, given the real bind
/// time, is refused; and the real bind time is a FILETIME no later than now (so the unit and time base are the
/// ones `GetProcessTimes` uses, in both directions).
#[cfg(windows)]
fn a_child_connection_names_the_child(bind: &str) {
    use std::process::{Command, Stdio};
    let _no_refused_connection_meanwhile =
        CHILD_PROCESSES.lock().unwrap_or_else(|p| p.into_inner());
    let listener = std::net::TcpListener::bind(bind).unwrap();
    let (mut child, accepted) = child_connecting_to(&listener);
    let (local, peer) = (
        accepted.local_addr().unwrap(),
        accepted.peer_addr().unwrap(),
    );
    let binder = imp::binder_of(local, peer)
        .unwrap()
        .expect("the child's row");
    let verdict = tcp_peer_owner(local, peer);
    // A process that began after the bind, held open while it is judged (ping sleeps for a minute; it is killed below).
    let mut later = Command::new("ping")
        .args(["-n", "60", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let own = crate::identity::os_account::own_account_id().unwrap();
    let later_binder = imp::Binder {
        pid: later.id(),
        bound: binder.bound,
    };
    let later_verdict = imp::owner_of(later_binder, &own, || Ok(Some(later_binder)));
    let _ = later.kill();
    let _ = later.wait();
    drop(child.stdin.take());
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill(); // after its stdin closed it exits by itself; a child that does not is ended here
    let _ = child.wait();
    let now = {
        let since_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        ((since_unix.as_secs() + 11_644_473_600) * 10_000_000
            + u64::from(since_unix.subsec_nanos()) / 100) as i64
    };
    assert_eq!(
        binder.pid,
        child.id(),
        "the row named this process, not the child that connected"
    );
    assert!(
        binder.bound > 0 && binder.bound <= now,
        "bind time {} against now {now}",
        binder.bound
    );
    assert_eq!(verdict, PeerOwner::Mine);
    assert!(
        matches!(later_verdict, PeerOwner::Unknown(ref why) if why.contains("created after the bind")),
        "a process begun after the bind was not refused for that reason: {later_verdict:?}"
    );
}

#[cfg(windows)]
#[test]
fn a_connection_from_a_child_process_names_the_child() {
    a_child_connection_names_the_child("127.0.0.1:0");
}

#[cfg(windows)]
#[test]
fn a_connection_from_a_child_process_over_ipv6_names_the_child() {
    a_child_connection_names_the_child("[::1]:0");
}
