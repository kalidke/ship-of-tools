//! Whose OS account is on the far end of a loopback TCP connection this process accepted. Decision 0031: every
//! browser-facing port Ship of Tools opens serves only the OS account that opened it. One lookup per accepted
//! connection, on a blocking thread:
//! - Linux: the kernel's TCP table (`/proc/net/tcp`, then `/proc/net/tcp6` for a client on a dual-stack socket);
//!   the peer's own row carries its uid.
//! - Windows: the owner-pid TCP table (`GetExtendedTcpTable`, IPv4 then IPv6 with v4-mapped addresses), then that
//!   process's token user SID.
//! - macOS: the kernel's TCP table (sysctl `net.inet.tcp.pcblist_n`, the one netstat reads); the peer's own
//!   connection carries the uid that created its socket, as on Linux.
//! Every failure refuses: only [`PeerOwner::Mine`] is served. [`serve_own`] is the one TCP accept loop that applies it.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerOwner {
    /// The peer's socket belongs to this process's OS account.
    Mine,
    /// It belongs to another account (`uid:<n>` or a SID), for the log line.
    Other(String),
    /// The lookup could not decide (no row, a closed peer, an API error, a table layout this build does not know): refused.
    Unknown(String),
}

/// The check [`serve`] runs on each connection; [`admit`] outside tests.
type Admit = fn(&'static str, std::net::SocketAddr, std::net::SocketAddr) -> bool;

/// `local` is the accepted stream's own address (the listener side), `peer` its remote address.
pub fn tcp_peer_owner(local: SocketAddr, peer: SocketAddr) -> PeerOwner {
    let Some(own) = crate::identity::os_account::own_account_id() else {
        return PeerOwner::Unknown("this process's own account is unreadable".into());
    };
    imp::lookup(local, peer, &own)
}

/// Refusals already warned of, by (listener, port, owner).
static DROPPED: Mutex<BTreeSet<(&'static str, u16, String)>> = Mutex::new(BTreeSet::new());

/// Serve only this account's connections. The first refusal per (listener, port, owner) is a warning; later ones
/// are debug, so a retrying stranger cannot flood the log and the operator still learns of it once. Bounded by
/// accounts x listeners on one box ("unknown" is one key, whatever its reason).
fn admit(listener: &'static str, local: SocketAddr, peer: SocketAddr) -> bool {
    let owner = tcp_peer_owner(local, peer);
    let key = match &owner {
        PeerOwner::Mine => return true,
        PeerOwner::Other(id) => id.clone(),
        PeerOwner::Unknown(_) => "unknown".to_string(),
    };
    let first = DROPPED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert((listener, local.port(), key));
    if first {
        tracing::warn!(listener, port = local.port(), ?owner, "closed a connection that is not this OS account's (logged once per account and port)");
    } else {
        tracing::debug!(listener, port = local.port(), ?owner, "closed a connection that is not this OS account's");
    }
    false
}

/// The only TCP accept loop Ship of Tools runs (ADR 0049, User isolation). Each accepted connection is checked in
/// its own task, on a blocking thread, before a byte is read: only this OS account's reaches `handle`; any other is
/// closed with nothing read or written (the first refusal per listener, port and owner is a warning). An accept
/// error is logged and retried after 50 ms. Runs until its future is dropped.
pub async fn serve_own<H, Fut>(listener: tokio::net::TcpListener, name: &'static str, handle: H)
where
    H: Fn(tokio::net::TcpStream) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    serve(listener, name, admit, handle).await
}

/// [`serve_own`] with the check as a parameter, so this module's tests can refuse a connection that is really ours.
async fn serve<H, Fut>(listener: tokio::net::TcpListener, name: &'static str, admit: Admit, handle: H)
where
    H: Fn(tokio::net::TcpStream) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let handle = Arc::new(handle);
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let handle = Arc::clone(&handle);
                tokio::spawn(async move {
                    let Ok(local) = stream.local_addr() else { return };
                    if !tokio::task::spawn_blocking(move || admit(name, local, peer)).await.unwrap_or(false) {
                        return; // `stream` drops here: closed with no byte read or written
                    }
                    handle(stream).await;
                });
            }
            Err(e) => {
                tracing::warn!(listener = name, port, error = %e, "page accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

/// The account the table names against this process's own: equal is mine, anything else is another's.
#[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
fn verdict(found: &str, own: &str) -> PeerOwner {
    if found == own {
        PeerOwner::Mine
    } else {
        PeerOwner::Other(found.to_string())
    }
}

/// One `/proc/net/tcp{,6}` endpoint, `"0100007F:1F90"` is 127.0.0.1:8080. The kernel prints each 32-bit address
/// word as a native-endian integer in hex and the port in hex host order; 8 hex digits are IPv4, 32 are IPv6 as
/// four words.
#[cfg(any(target_os = "linux", test))]
fn parse_endpoint(s: &str) -> Option<SocketAddr> {
    let (addr, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let word = |w: &str| u32::from_str_radix(w, 16).ok().map(u32::to_ne_bytes);
    match addr.len() {
        8 => Some(SocketAddr::from((std::net::Ipv4Addr::from(word(addr)?), port))),
        32 => {
            let mut octets = [0u8; 16];
            for (i, chunk) in octets.chunks_exact_mut(4).enumerate() {
                chunk.copy_from_slice(&word(addr.get(i * 8..i * 8 + 8)?)?);
            }
            Some(SocketAddr::from((std::net::Ipv6Addr::from(octets), port)))
        }
        _ => None,
    }
}

/// The uid in the row of `table` whose local endpoint is `peer` and remote endpoint `listener` (the peer's own
/// end of the connection), if there is one. It reads only a live, owned row (ESTABLISHED with an inode), because a
/// closing orphan carries uid 0 and no inode.
#[cfg(any(target_os = "linux", test))]
fn uid_of_row(table: &str, peer: SocketAddr, listener: SocketAddr) -> Option<u32> {
    table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        if parse_endpoint(f.get(1)?)? == peer
            && parse_endpoint(f.get(2)?)? == listener
            && *f.get(3)? == "01"
            && *f.get(9)? != "0"
        {
            f.get(7)?.parse().ok()
        } else {
            None
        }
    })
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{uid_of_row, verdict, PeerOwner};
    use std::net::SocketAddr;

    fn mapped(a: SocketAddr) -> SocketAddr {
        match a {
            SocketAddr::V4(v4) => SocketAddr::from((v4.ip().to_ipv6_mapped(), v4.port())),
            v6 => v6,
        }
    }

    pub(super) fn lookup(local: SocketAddr, peer: SocketAddr, own: &str) -> PeerOwner {
        let mut tables: Vec<(&str, SocketAddr, SocketAddr)> = Vec::new();
        if peer.is_ipv4() {
            tables.push(("/proc/net/tcp", peer, local));
        }
        tables.push(("/proc/net/tcp6", mapped(peer), mapped(local)));
        for (path, peer, local) in tables {
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => return PeerOwner::Unknown(format!("{path}: {e}")),
            };
            if let Some(uid) = uid_of_row(&text, peer, local) {
                return verdict(&format!("uid:{uid}"), own);
            }
        }
        PeerOwner::Unknown("no TCP table row for the peer (it may have closed)".into())
    }
}

/// The macOS TCP table, `net.inet.tcp.pcblist_n`, read as XNU writes it (`get_pcblist_n`, bsd/netinet/in_pcblist.c):
/// an `xinpgen`, then per connection an `xinpcb_n`, `xsocket_n`, two `xsockbuf_n`, an `xsockstat_n` and an
/// `xtcpcb_n`, each starting with its u32 length and u32 kind and padded to 8 bytes, then a closing `xinpgen`.
/// Compiled for tests on every platform, so the walk is tested where the tests run.
#[cfg(any(target_os = "macos", test))]
mod pcblist_n;

#[cfg(target_os = "macos")]
mod imp {
    use super::{pcblist_n, PeerOwner};
    use std::net::SocketAddr;

    /// `net.inet.tcp.pcblist_n`, the TCP table netstat reads; any account may read it.
    fn table() -> Result<Vec<u8>, String> {
        let name = c"net.inet.tcp.pcblist_n";
        // The size the kernel names allows for an eighth more connections; a table that outgrows even that between
        // the two calls fails with ENOMEM and is asked for again.
        for _ in 0..3 {
            let mut len: libc::size_t = 0;
            // SAFETY: a null buffer asks only for the size, which the kernel writes to `len`.
            if unsafe { libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) } != 0 {
                return Err(format!("net.inet.tcp.pcblist_n: {}", std::io::Error::last_os_error()));
            }
            let mut buf = vec![0u8; len];
            // SAFETY: `buf` has `len` writable bytes; the kernel writes at most that many and stores the count in `len`.
            if unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) } == 0 {
                buf.truncate(len);
                return Ok(buf);
            }
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ENOMEM) {
                return Err(format!("net.inet.tcp.pcblist_n: {e}"));
            }
        }
        Err("net.inet.tcp.pcblist_n kept growing past its size".into())
    }

    pub(super) fn lookup(local: SocketAddr, peer: SocketAddr, own: &str) -> PeerOwner {
        match table() {
            Ok(buf) => pcblist_n::owner(&buf, peer, local, own),
            Err(e) => PeerOwner::Unknown(e),
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::{verdict, PeerOwner};
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, MIB_TCP_STATE_ESTAB,
        TCP_TABLE_OWNER_PID_CONNECTIONS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    /// The owner-pid TCP table for one address family, as 8-byte-aligned words: a `u32` row count, then the rows.
    /// `af` is 2 (AF_INET) or 23 (AF_INET6), literal so no WinSock feature is needed.
    fn table(af: u32) -> io::Result<Vec<u64>> {
        let mut size = 0u32;
        for _ in 0..4 {
            let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
            let rc = unsafe {
                GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, af, TCP_TABLE_OWNER_PID_CONNECTIONS, 0)
            };
            if rc == 0 {
                return Ok(buf);
            }
            if rc != ERROR_INSUFFICIENT_BUFFER {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
        }
        Err(io::Error::other("the TCP table kept growing"))
    }

    /// The rows of a table from [`table`]: the count the table states, bounded by the bytes the buffer holds.
    fn rows<R: Copy>(buf: &[u64]) -> Vec<R> {
        let bytes = buf.len() * 8;
        let base = buf.as_ptr().cast::<u8>();
        let stated = unsafe { std::ptr::read_unaligned(base.cast::<u32>()) } as usize;
        let count = stated.min((bytes - 4) / std::mem::size_of::<R>());
        (0..count)
            .map(|i| unsafe { std::ptr::read_unaligned(base.add(4 + i * std::mem::size_of::<R>()).cast::<R>()) })
            .collect()
    }

    fn v4(addr: u32, port: u32) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(addr.to_ne_bytes())), u16::from_be(port as u16))
    }

    fn v6(addr: [u8; 16], port: u32) -> SocketAddr {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::from(addr)), u16::from_be(port as u16))
    }

    fn mapped(a: SocketAddr) -> SocketAddr {
        match a {
            SocketAddr::V4(a) => SocketAddr::new(IpAddr::V6(a.ip().to_ipv6_mapped()), a.port()),
            v6 => v6,
        }
    }

    /// The pid owning the peer's end of the connection: the ESTABLISHED row whose local endpoint is `peer` and remote
    /// `listener`.
    fn owning_pid(listener: SocketAddr, peer: SocketAddr) -> io::Result<Option<u32>> {
        if let (SocketAddr::V4(_), SocketAddr::V4(_)) = (listener, peer) {
            let found = rows::<MIB_TCPROW_OWNER_PID>(&table(2)?)
                .into_iter()
                .find(|r| {
                    r.dwState == MIB_TCP_STATE_ESTAB as u32
                        && v4(r.dwLocalAddr, r.dwLocalPort) == peer
                        && v4(r.dwRemoteAddr, r.dwRemotePort) == listener
                });
            if let Some(r) = found {
                return Ok(Some(r.dwOwningPid));
            }
        }
        let (listener, peer) = (mapped(listener), mapped(peer));
        let found = rows::<MIB_TCP6ROW_OWNER_PID>(&table(23)?)
            .into_iter()
            .find(|r| {
                r.dwState == MIB_TCP_STATE_ESTAB as u32
                    && v6(r.ucLocalAddr, r.dwLocalPort) == peer
                    && v6(r.ucRemoteAddr, r.dwRemotePort) == listener
            });
        Ok(found.map(|r| r.dwOwningPid))
    }

    /// The token user of process `pid` against this process's own, with the process held open throughout. A pid is
    /// unique only while its process lives, so `still_owner` asks the TCP table again once the handle is held: the
    /// held handle keeps the pid from being reused, and only an unchanged answer proves the pid is the connection's.
    /// Another account's process usually cannot be opened, which is a refusal like any other failure.
    pub(super) fn owner_of(pid: u32, own: &str, still_owner: impl FnOnce() -> io::Result<Option<u32>>) -> PeerOwner {
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            return PeerOwner::Unknown(format!("OpenProcess({pid}): {}", io::Error::last_os_error()));
        }
        let sid = match still_owner() {
            Ok(Some(again)) if again == pid => Some(crate::host::sid_string_from_process(h)),
            _ => None,
        };
        unsafe {
            CloseHandle(h);
        }
        match sid {
            Some(Ok(sid)) => verdict(&sid, own),
            Some(Err(e)) => PeerOwner::Unknown(format!("token of {pid}: {e}")),
            None => PeerOwner::Unknown("the peer's connection changed during the lookup".into()),
        }
    }

    /// [`owner_of`] without the revalidation, for a pid the caller already holds.
    #[cfg(test)]
    pub(super) fn pid_owner(pid: u32, own: &str) -> PeerOwner {
        owner_of(pid, own, || Ok(Some(pid)))
    }

    pub(super) fn lookup(local: SocketAddr, peer: SocketAddr, own: &str) -> PeerOwner {
        match owning_pid(local, peer) {
            Ok(Some(pid)) => owner_of(pid, own, || owning_pid(local, peer)),
            Ok(None) => PeerOwner::Unknown("no TCP table row for the peer (it may have closed)".into()),
            Err(e) => PeerOwner::Unknown(format!("GetExtendedTcpTable: {e}")),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::PeerOwner;
    use std::net::SocketAddr;

    pub(super) fn lookup(_local: SocketAddr, _peer: SocketAddr, _own: &str) -> PeerOwner {
        PeerOwner::Unknown("no owner lookup on this platform".into())
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(uid_of_row(CAPTURE, loopback(0xA8B2), loopback(0x9931)), Some(1028));
        assert_eq!(uid_of_row(CAPTURE, client, loopback(1)), None);
        assert_eq!(parse_endpoint("0100007F:1F90"), Some(loopback(8080)));
        assert_eq!(parse_endpoint("0100007F"), None);
        assert_eq!(verdict("uid:0", "uid:1001"), PeerOwner::Other("uid:0".into()));
        assert_eq!(verdict("uid:1001", "uid:1001"), PeerOwner::Mine);
    }

    #[test]
    fn a_closing_or_ownerless_row_names_no_owner() {
        let (client, server) = (loopback(0x80EC), loopback(0xE387));
        let header = CAPTURE.lines().next().unwrap();
        let row = CAPTURE.lines().find(|l| l.contains("0100007F:80EC 0100007F:E387")).unwrap();
        // A FIN_WAIT2/TIME_WAIT orphan: state 06, uid 0, inode 0.
        let closing = row.replace(" 01 ", " 06 ").replace("1001        0 2335972813", "   0        0 0");
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
        let (local, peer) = (accepted.local_addr().unwrap(), accepted.peer_addr().unwrap());
        assert_eq!(tcp_peer_owner(local, peer), PeerOwner::Mine);
        assert!(admit("test", local, peer));
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
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_in = Arc::clone(&ran);
        let task = tokio::spawn(serve(listener, "test", |_, _, _| false, move |mut s| {
            let ran = Arc::clone(&ran_in);
            async move {
                ran.fetch_add(1, SeqCst);
                let _ = s.write_all(b"served").await;
            }
        }));
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut raw = Vec::new();
        // A reset counts as the end.
        let _ = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut raw))
            .await
            .expect("the refused connection must close");
        assert!(raw.is_empty(), "got: {}", String::from_utf8_lossy(&raw));
        assert_eq!(ran.load(SeqCst), 0, "a refused connection must never reach the handler");
        task.abort();
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
        assert_eq!(imp::pid_owner(std::process::id(), &own), PeerOwner::Mine);
        // Pid 4 is the System process: its token is another account's, or it cannot be opened at all.
        assert_ne!(imp::pid_owner(4, &own), PeerOwner::Mine);
    }

    #[cfg(windows)]
    #[test]
    fn a_connection_whose_owner_changed_is_refused() {
        let own = crate::identity::os_account::own_account_id().unwrap();
        let me = std::process::id();
        assert_eq!(imp::owner_of(me, &own, || Ok(Some(me))), PeerOwner::Mine);
        assert_ne!(imp::owner_of(me, &own, || Ok(None)), PeerOwner::Mine);
        assert_ne!(imp::owner_of(me, &own, || Ok(Some(4))), PeerOwner::Mine);
    }
}
