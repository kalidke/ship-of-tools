//! Whose OS account is on the far end of a loopback TCP connection this process accepted. Decision 0031: every
//! browser-facing port Ship of Tools opens serves only the OS account that opened it. One lookup per accepted
//! connection, on a blocking thread:
//! - Linux: the kernel's TCP table (`/proc/net/tcp`, then `/proc/net/tcp6` for a client on a dual-stack socket);
//!   the peer's own row carries its uid.
//! - Windows: the owner-module TCP table (`GetExtendedTcpTable`, IPv4 then IPv6 with v4-mapped addresses): the binding
//!   process and the bind time; a process created after the bind is a recycled pid and refused; then that process's
//!   token user SID.
//! - macOS: the kernel's TCP table (sysctl `net.inet.tcp.pcblist_n`, the one netstat reads); the peer's own
//!   connection carries the uid that created its socket, as on Linux.
//! Every failure refuses: only [`PeerOwner::Mine`] is served. [`serve_own`] is the one TCP accept loop of the Rust
//! processes, and applies it.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PeerOwner {
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
fn tcp_peer_owner(local: SocketAddr, peer: SocketAddr) -> PeerOwner {
    let Some(own) = crate::identity::os_account::own_account_id() else {
        return PeerOwner::Unknown("this process's own account is unreadable".into());
    };
    imp::lookup(local, peer, &own)
}

/// How many owner lookups one listener runs at once. Each lookup reads a whole kernel table on a thread of the
/// process's shared blocking pool, so another account opening many connections to a page port must not be able to
/// fill that pool, which the rest of the daemon uses too; the connections beyond it wait their turn.
const MAX_LOOKUPS: usize = 8;

/// Refusals already warned of, by (listener, port, owner).
static DROPPED: Mutex<BTreeSet<(&'static str, u16, String)>> = Mutex::new(BTreeSet::new());

/// Serve only this account's connections. The first refusal per (listener, port, owner) is a warning; later ones
/// are debug, so a retrying stranger cannot flood the log and the operator still learns of it once. Bounded by
/// accounts x listeners on one box ("unknown" is one key, whatever its reason).
fn admit(listener: &'static str, local: SocketAddr, peer: SocketAddr) -> bool {
    owner_decision(listener, local, tcp_peer_owner(local, peer))
}

fn owner_decision(listener: &'static str, local: SocketAddr, owner: PeerOwner) -> bool {
    let key = match &owner {
        PeerOwner::Mine => return true,
        PeerOwner::Other(id) => id.clone(),
        PeerOwner::Unknown(_) => "unknown".to_string(),
    };
    let first =
        DROPPED
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((listener, local.port(), key));
    let why = match owner {
        PeerOwner::Unknown(_) => "closed a connection whose owner could not be determined",
        _ => "closed a connection that is not this OS account's",
    };
    if first {
        tracing::warn!(
            listener,
            port = local.port(),
            ?owner,
            "{why} (logged once per account and port)"
        );
    } else {
        tracing::debug!(listener, port = local.port(), ?owner, "{why}");
    }
    false
}

/// The only TCP accept loop Ship of Tools' Rust processes run (ADR 0049, User isolation); the Julia page servers
/// (Pluto, `wglshow`) listen on their own ports and are locked by a secret instead. Each accepted connection is checked in
/// its own task, on a blocking thread (at most `MAX_LOOKUPS` at a time per listener), before a byte is read: only this OS account's reaches `handle`; any other is
/// closed with nothing read or written (the first refusal per listener, port and owner is a warning). An accept
/// error is logged and retried after 50 ms. Runs until its future is dropped.
pub async fn serve_own<H, Fut>(listener: tokio::net::TcpListener, name: &'static str, handle: H)
where
    H: Fn(tokio::net::TcpStream) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    serve(listener, name, admit, handle).await
}

/// On Windows, a socket a process holds is inherited by every child that process starts, and tokio's accepted sockets
/// are inheritable: a child started while a connection is being refused would keep it open at the other end until it
/// ends. So an accepted socket is made non-inheritable the moment it is accepted, before the check. A failure is
/// logged at debug and does not refuse: the check still runs. Nothing to do elsewhere.
#[cfg(windows)]
fn no_inherit(stream: &tokio::net::TcpStream) {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    if unsafe { SetHandleInformation(stream.as_raw_socket() as _, HANDLE_FLAG_INHERIT, 0) } == 0 {
        tracing::debug!(error = %std::io::Error::last_os_error(), "an accepted socket could not be made non-inheritable");
    }
}
#[cfg(not(windows))]
fn no_inherit(_: &tokio::net::TcpStream) {}

/// [`serve_own`] with the check as a parameter, so this module's tests can refuse a connection that is really ours.
async fn serve<H, Fut>(
    listener: tokio::net::TcpListener,
    name: &'static str,
    admit: Admit,
    handle: H,
) where
    H: Fn(tokio::net::TcpStream) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let handle = Arc::new(handle);
    let lookups = Arc::new(tokio::sync::Semaphore::new(MAX_LOOKUPS));
    loop {
        // The turn comes before the accept, so a flood beyond the bound waits in the kernel's backlog and not as accepted
        // streams, each holding one of the process's descriptors.
        let Ok(turn) = Arc::clone(&lookups).acquire_owned().await else {
            return;
        };
        #[allow(
            clippy::disallowed_methods,
            reason = "listener: page (TCP): every connection is checked for its owner before use (ADR 0049, User isolation)"
        )]
        let accepted = listener.accept().await;
        match accepted {
            Ok((stream, peer)) => {
                no_inherit(&stream);
                let handle = Arc::clone(&handle);
                tokio::spawn(async move {
                    let Ok(local) = stream.local_addr() else {
                        return;
                    };
                    let admitted = tokio::task::spawn_blocking(move || admit(name, local, peer))
                        .await
                        .unwrap_or(false);
                    drop(turn);
                    if !admitted {
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
        8 => Some(SocketAddr::from((
            std::net::Ipv4Addr::from(word(addr)?),
            port,
        ))),
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
            if unsafe {
                libc::sysctlbyname(
                    name.as_ptr(),
                    std::ptr::null_mut(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            } != 0
            {
                return Err(format!(
                    "net.inet.tcp.pcblist_n: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut buf = vec![0u8; len];
            // SAFETY: `buf` has `len` writable bytes; the kernel writes at most that many and stores the count in `len`.
            if unsafe {
                libc::sysctlbyname(
                    name.as_ptr(),
                    buf.as_mut_ptr().cast(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            } == 0
            {
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
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, FILETIME};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_MODULE, MIB_TCPROW_OWNER_MODULE,
        MIB_TCP_STATE_ESTAB, TCP_TABLE_OWNER_MODULE_CONNECTIONS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// The process that issued the context bind of a connection, and when (FILETIME ticks). The table names the
    /// binder and never updates it: a socket a process bound and handed to a child still names the parent, whose pid
    /// Windows may later give to any process. The timestamp is what tells the two apart.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Binder {
        pub(super) pid: u32,
        pub(super) bound: i64,
    }

    /// The owner-module TCP table for one address family, as 8-byte-aligned words: a `u32` row count, then the rows.
    /// `af` is 2 (AF_INET) or 23 (AF_INET6), literal so no WinSock feature is needed.
    fn table(af: u32) -> io::Result<Vec<u64>> {
        let mut size = 0u32;
        for _ in 0..4 {
            let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
            let rc = unsafe {
                GetExtendedTcpTable(
                    buf.as_mut_ptr().cast(),
                    &mut size,
                    0,
                    af,
                    TCP_TABLE_OWNER_MODULE_CONNECTIONS,
                    0,
                )
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

    /// The rows of a table from [`table`]: the count the table states, bounded by the bytes the buffer holds. The
    /// rows start after the count at the row type's own alignment (8 for the module rows, which hold an `i64`).
    fn rows<R: Copy>(buf: &[u64]) -> Vec<R> {
        let bytes = buf.len() * 8;
        let base = buf.as_ptr().cast::<u8>();
        let stated = unsafe { std::ptr::read_unaligned(base.cast::<u32>()) } as usize;
        let start = 4usize.next_multiple_of(std::mem::align_of::<R>());
        let count = stated.min(bytes.saturating_sub(start) / std::mem::size_of::<R>());
        (0..count)
            .map(|i| unsafe {
                std::ptr::read_unaligned(base.add(start + i * std::mem::size_of::<R>()).cast::<R>())
            })
            .collect()
    }

    fn v4(addr: u32, port: u32) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V4(Ipv4Addr::from(addr.to_ne_bytes())),
            u16::from_be(port as u16),
        )
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

    /// The binder of the peer's end of the connection: the ESTABLISHED row whose local endpoint is `peer` and remote
    /// `listener`.
    pub(super) fn binder_of(listener: SocketAddr, peer: SocketAddr) -> io::Result<Option<Binder>> {
        if let (SocketAddr::V4(_), SocketAddr::V4(_)) = (listener, peer) {
            let found = rows::<MIB_TCPROW_OWNER_MODULE>(&table(2)?)
                .into_iter()
                .find(|r| {
                    r.dwState == MIB_TCP_STATE_ESTAB as u32
                        && v4(r.dwLocalAddr, r.dwLocalPort) == peer
                        && v4(r.dwRemoteAddr, r.dwRemotePort) == listener
                });
            if let Some(r) = found {
                return Ok(Some(Binder {
                    pid: r.dwOwningPid,
                    bound: r.liCreateTimestamp,
                }));
            }
        }
        let (listener, peer) = (mapped(listener), mapped(peer));
        let found = rows::<MIB_TCP6ROW_OWNER_MODULE>(&table(23)?)
            .into_iter()
            .find(|r| {
                r.dwState == MIB_TCP_STATE_ESTAB as u32
                    && v6(r.ucLocalAddr, r.dwLocalPort) == peer
                    && v6(r.ucRemoteAddr, r.dwRemotePort) == listener
            });
        Ok(found.map(|r| Binder {
            pid: r.dwOwningPid,
            bound: r.liCreateTimestamp,
        }))
    }

    /// The creation time of the process `h` holds, in FILETIME ticks.
    fn created(h: windows_sys::Win32::Foundation::HANDLE) -> io::Result<i64> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut create, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        if unsafe { GetProcessTimes(h, &mut create, &mut exit, &mut kernel, &mut user) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((i64::from(create.dwHighDateTime) << 32) | i64::from(create.dwLowDateTime))
    }

    /// The token user of the binder's process against this process's own, with the process held open throughout.
    /// A pid is unique only while its process lives, and the table never updates the binder, so two checks stand
    /// between a pid and a verdict. Holding the handle keeps the pid from being reused from now on, and
    /// `still_binder` asks the table again to prove the row did not change meanwhile. The process must also have been
    /// created no later than the bind: a process created after it can only be a stranger's pid, recycled. The two
    /// times may come from clocks that tick at different intervals, so a legitimate process that binds within its first
    /// tick could be refused (it fails closed, and an equal stamp is admitted); any tolerance would let a recycled pid
    /// pass for the same window, so there is none. Another account's process usually cannot be opened, which is a
    /// refusal like any other failure.
    pub(super) fn owner_of(
        binder: Binder,
        own: &str,
        still_binder: impl FnOnce() -> io::Result<Option<Binder>>,
    ) -> PeerOwner {
        let pid = binder.pid;
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            return PeerOwner::Unknown(format!(
                "OpenProcess({pid}): {}",
                io::Error::last_os_error()
            ));
        }
        let verdict_for = || {
            match still_binder() {
            Ok(Some(again)) if again == binder => match created(h) {
                Ok(born) if born > binder.bound => Err("the process holding the binder's pid was created after the bind: a recycled id".to_string()),
                Ok(_) => crate::host::sid_string_from_process(h).map_err(|e| format!("token of {pid}: {e}")),
                Err(e) => Err(format!("GetProcessTimes({pid}): {e}")),
            },
            _ => Err("the peer's connection changed during the lookup".to_string()),
        }
        };
        let sid = verdict_for();
        unsafe {
            CloseHandle(h);
        }
        match sid {
            Ok(sid) => verdict(&sid, own),
            Err(why) => PeerOwner::Unknown(why),
        }
    }

    /// [`owner_of`] without the revalidation, for a process the caller already holds and a bind the caller names.
    #[cfg(test)]
    pub(super) fn pid_owner(pid: u32, bound: i64, own: &str) -> PeerOwner {
        let binder = Binder { pid, bound };
        owner_of(binder, own, || Ok(Some(binder)))
    }

    pub(super) fn lookup(local: SocketAddr, peer: SocketAddr, own: &str) -> PeerOwner {
        match binder_of(local, peer) {
            Ok(Some(binder)) => owner_of(binder, own, || binder_of(local, peer)),
            Ok(None) => {
                PeerOwner::Unknown("no TCP table row for the peer (it may have closed)".into())
            }
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
#[path = "tests.rs"]
mod tests;
