//! Whose OS account is on the far end of a loopback TCP connection this process accepted. Decision 0031: every
//! browser-facing port Ship of Tools opens serves only the OS account that opened it. One lookup per accepted
//! connection, on a blocking thread:
//! - Linux: the kernel's TCP table (`/proc/net/tcp`, then `/proc/net/tcp6` for a client on a dual-stack socket);
//!   the peer's own row carries its uid.
//! - Windows: the owner-pid TCP table (`GetExtendedTcpTable`, IPv4 then IPv6 with v4-mapped addresses), then that
//!   process's token user SID.
//! - macOS: the kernel's TCP table (sysctl `net.inet.tcp.pcblist_n`, the one netstat reads); the peer's own
//!   connection carries the uid that created its socket, as on Linux.
//! Every failure refuses: only [`PeerOwner::Mine`] is served.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerOwner {
    /// The peer's socket belongs to this process's OS account.
    Mine,
    /// It belongs to another account (`uid:<n>` or a SID), for the log line.
    Other(String),
    /// The lookup could not decide (no row, a closed peer, an API error, a table layout this build does not know): refused.
    Unknown(String),
}

/// The check every page listener runs; [`admit`] outside tests.
pub type Admit = fn(&'static str, std::net::SocketAddr, std::net::SocketAddr) -> bool;

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
pub fn admit(listener: &'static str, local: SocketAddr, peer: SocketAddr) -> bool {
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
mod pcblist_n {
    use super::{verdict, PeerOwner};
    use std::mem::{offset_of, size_of};
    use std::net::{IpAddr, SocketAddr};

    // <sys/socketvar.h>, <netinet/in_pcb.h>, <netinet/tcp_fsm.h>.
    const XSO_SOCKET: u32 = 0x001;
    const XSO_INPCB: u32 = 0x010;
    const XSO_TCPCB: u32 = 0x020;
    const INP_IPV4: u8 = 0x1;
    const INP_IPV6: u8 = 0x2;
    const TCPS_ESTABLISHED: i32 = 4;

    /// A transcribed record: integer fields only and no implicit padding, so every byte pattern is a value and
    /// every byte of a value is initialized.
    unsafe trait Plain: Copy {}

    // Transcribed from XNU (identical from xnu-8792, macOS 13, through xnu-12377, macOS 26) under its
    // `#pragma pack(4)`: each field up to the last one read here, C's implicit padding spelled out, the rest
    // one byte array. A kernel whose record differs in length is refused, never misread.

    /// struct xinpgen.
    #[allow(dead_code)]
    #[repr(C, packed(4))]
    #[derive(Clone, Copy)]
    struct XinpGen {
        xig_len: u32,
        xig_count: u32,
        xig_gen: u64,
        xig_sogen: u64,
    }

    /// struct xinpcb_n. Ports are in network byte order; an address is an in6_addr, or an in_addr_4in6 whose
    /// last four bytes are the IPv4 address (`inp_vflag` says which).
    #[allow(dead_code)]
    #[repr(C, packed(4))]
    #[derive(Clone, Copy)]
    struct XinpcbN {
        xi_len: u32,
        xi_kind: u32,
        xi_inpp: u64,
        inp_fport: u16,
        inp_lport: u16,
        inp_ppcb: u64,
        inp_gencnt: u64,
        inp_flags: i32,
        inp_flow: u32,
        inp_vflag: u8,
        inp_ip_ttl: u8,
        inp_ip_p: u8,
        _pad0: u8,
        inp_dependfaddr: [u8; 16],
        inp_dependladdr: [u8; 16],
        /// inp_depend4, inp_depend6, inp_flowhash, inp_flags2.
        _rest: [u8; 24],
    }

    /// struct xsocket_n.
    #[allow(dead_code)]
    #[repr(C, packed(4))]
    #[derive(Clone, Copy)]
    struct XsocketN {
        xso_len: u32,
        xso_kind: u32,
        xso_so: u64,
        so_type: i16,
        _pad0: [u8; 2],
        so_options: u32,
        so_linger: i16,
        so_state: i16,
        so_pcb: u64,
        xso_protocol: i32,
        xso_family: i32,
        so_qlen: i16,
        so_incqlen: i16,
        so_qlimit: i16,
        so_timeo: i16,
        so_error: u16,
        _pad1: [u8; 2],
        so_pgid: i32,
        so_oobmark: u32,
        so_uid: u32,
        /// so_last_pid through xso_filter_flags.
        _rest: [u8; 36],
    }

    /// struct xtcpcb_n.
    #[allow(dead_code)]
    #[repr(C, packed(4))]
    #[derive(Clone, Copy)]
    struct XtcpcbN {
        xt_len: u32,
        xt_kind: u32,
        t_segq: u64,
        t_dupacks: i32,
        t_timer: [i32; 4],
        t_state: i32,
        /// t_flags through snd_ssthresh_prev.
        _rest: [u8; 164],
    }

    // SAFETY: integer fields only; the assertions below pin each one with no gap left for implicit padding.
    unsafe impl Plain for XinpGen {}
    unsafe impl Plain for XinpcbN {}
    unsafe impl Plain for XsocketN {}
    unsafe impl Plain for XtcpcbN {}

    const _: () = {
        assert!(size_of::<XinpGen>() == 24);
        assert!(offset_of!(XinpGen, xig_sogen) == 16);
        assert!(size_of::<XinpcbN>() == 104);
        assert!(offset_of!(XinpcbN, inp_fport) == 16);
        assert!(offset_of!(XinpcbN, inp_lport) == 18);
        assert!(offset_of!(XinpcbN, inp_ppcb) == 20);
        assert!(offset_of!(XinpcbN, inp_gencnt) == 28);
        assert!(offset_of!(XinpcbN, inp_vflag) == 44);
        assert!(offset_of!(XinpcbN, inp_dependfaddr) == 48);
        assert!(offset_of!(XinpcbN, inp_dependladdr) == 64);
        assert!(offset_of!(XinpcbN, _rest) == 80);
        assert!(size_of::<XsocketN>() == 104);
        assert!(offset_of!(XsocketN, xso_so) == 8);
        assert!(offset_of!(XsocketN, so_options) == 20);
        assert!(offset_of!(XsocketN, so_pcb) == 28);
        assert!(offset_of!(XsocketN, xso_protocol) == 36);
        assert!(offset_of!(XsocketN, xso_family) == 40);
        assert!(offset_of!(XsocketN, so_pgid) == 56);
        assert!(offset_of!(XsocketN, so_uid) == 64);
        assert!(offset_of!(XsocketN, _rest) == 68);
        assert!(size_of::<XtcpcbN>() == 204);
        assert!(offset_of!(XtcpcbN, t_timer) == 20);
        assert!(offset_of!(XtcpcbN, t_state) == 36);
        assert!(offset_of!(XtcpcbN, _rest) == 40);
    };

    fn u32_at(buf: &[u8], off: usize) -> Option<u32> {
        Some(u32::from_ne_bytes(buf.get(off..off.checked_add(4)?)?.try_into().ok()?))
    }

    /// The record of type `T` at `off`, whose length field said `len`: any other length is a layout this code
    /// does not know.
    fn record<T: Plain>(buf: &[u8], off: usize, len: usize) -> Result<T, String> {
        let size = size_of::<T>();
        if len != size {
            return Err(format!("pcblist_n: a {} is {len} bytes on this macOS, {size} here", std::any::type_name::<T>()));
        }
        let bytes = buf.get(off..off + size).ok_or("pcblist_n: truncated")?;
        // SAFETY: `bytes` holds `size_of::<T>()` bytes and `T: Plain` makes any bytes a valid `T`; the read is
        // unaligned by design.
        Ok(unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) })
    }

    /// One connection's records.
    struct Pcb {
        inp: XinpcbN,
        so: Option<XsocketN>,
        tcp: Option<XtcpcbN>,
    }

    /// An endpoint as `inp_vflag` says it is stored; a v4-mapped IPv6 address reads as IPv4.
    fn endpoint(vflag: u8, addr: [u8; 16], port: u16) -> Option<(IpAddr, u16)> {
        let ip = if vflag & INP_IPV4 != 0 {
            IpAddr::from([addr[12], addr[13], addr[14], addr[15]])
        } else if vflag & INP_IPV6 != 0 {
            IpAddr::from(addr)
        } else {
            return None;
        };
        Some((ip.to_canonical(), u16::from_be(port)))
    }

    /// The creator's uid if `pcb` is the peer's own end (local `peer`, foreign `listener`), established, and backed
    /// by a kernel socket. `so_uid` is that socket's creator; the handle is a hash of the socket, zero exactly when
    /// there is none (a connection whose socket is gone, a user-space-stack flow), and those records say uid 0, so
    /// without the handle they would pass as root's.
    fn creator(pcb: Pcb, peer: (IpAddr, u16), listener: (IpAddr, u16)) -> Result<Option<u32>, String> {
        let (Some(so), Some(tcp)) = (pcb.so, pcb.tcp) else {
            return Err("pcblist_n: a connection without its socket or TCP record".into());
        };
        let inp = pcb.inp;
        let live = endpoint(inp.inp_vflag, inp.inp_dependladdr, inp.inp_lport) == Some(peer)
            && endpoint(inp.inp_vflag, inp.inp_dependfaddr, inp.inp_fport) == Some(listener)
            && { tcp.t_state } == TCPS_ESTABLISHED
            && { so.xso_so } != 0;
        Ok(live.then_some(so.so_uid))
    }

    /// Whose account created the peer's end of the connection `listener` accepted from `peer`, in the table `buf`.
    /// The first live match decides, as on Linux; a table that does not parse to its last byte decides nothing.
    pub(super) fn owner(buf: &[u8], peer: SocketAddr, listener: SocketAddr, own: &str) -> PeerOwner {
        match uid_of_pcb(buf, peer, listener) {
            Ok(Some(uid)) => verdict(&format!("uid:{uid}"), own),
            Ok(None) => PeerOwner::Unknown("no live TCP connection from the peer (it may have closed)".into()),
            Err(e) => PeerOwner::Unknown(e),
        }
    }

    fn uid_of_pcb(buf: &[u8], peer: SocketAddr, listener: SocketAddr) -> Result<Option<u32>, String> {
        let (peer, listener) = ((peer.ip().to_canonical(), peer.port()), (listener.ip().to_canonical(), listener.port()));
        let gen = size_of::<XinpGen>();
        record::<XinpGen>(buf, 0, u32_at(buf, 0).ok_or("pcblist_n: empty")? as usize)?;
        let mut off = gen;
        let (mut pcb, mut found): (Option<Pcb>, Option<u32>) = (None, None);
        loop {
            let len = u32_at(buf, off).ok_or("pcblist_n: truncated")? as usize;
            if len == gen {
                // The closing xinpgen, which must end the table.
                if off + gen != buf.len() {
                    return Err("pcblist_n: bytes after the closing record".into());
                }
                if let Some(p) = pcb.take() {
                    found = found.or(creator(p, peer, listener)?);
                }
                return Ok(found);
            }
            if len < 8 {
                return Err(format!("pcblist_n: a {len}-byte record"));
            }
            let kind = u32_at(buf, off + 4).ok_or("pcblist_n: truncated")?;
            let next = off.checked_add(len.next_multiple_of(8)).filter(|&n| n <= buf.len()).ok_or("pcblist_n: truncated")?;
            match kind {
                XSO_INPCB => {
                    if let Some(p) = pcb.take() {
                        found = found.or(creator(p, peer, listener)?);
                    }
                    pcb = Some(Pcb { inp: record(buf, off, len)?, so: None, tcp: None });
                }
                XSO_SOCKET => {
                    let p = pcb.as_mut().filter(|p| p.so.is_none()).ok_or("pcblist_n: a socket record out of place")?;
                    p.so = Some(record(buf, off, len)?);
                }
                XSO_TCPCB => {
                    let p = pcb.as_mut().filter(|p| p.tcp.is_none()).ok_or("pcblist_n: a TCP record out of place")?;
                    p.tcp = Some(record(buf, off, len)?);
                }
                _ => {}
            }
            off = next;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The socket record a connection carries in the table.
        #[derive(Clone, Copy)]
        enum Sock {
            /// A kernel socket, with its creator's uid.
            Kernel(u32),
            /// No socket: the kernel leaves all but the length and kind zero.
            Absent,
            /// A user-space-stack flow, as `nstat_userland_to_xsocket_n` writes it: TCP, nothing else.
            Userland,
        }

        fn put<T: Plain>(buf: &mut Vec<u8>, t: T) {
            // SAFETY: `T: Plain` has no padding, so all its bytes are initialized.
            let bytes = unsafe { std::slice::from_raw_parts((&t as *const T).cast::<u8>(), size_of::<T>()) };
            buf.extend_from_slice(bytes);
            buf.resize(buf.len().next_multiple_of(8), 0);
        }

        /// A record this code only skips: its length, its kind, zeros.
        fn skipped(buf: &mut Vec<u8>, len: u32, kind: u32) {
            buf.extend_from_slice(&len.to_ne_bytes());
            buf.extend_from_slice(&kind.to_ne_bytes());
            buf.resize(buf.len() + (len as usize - 8).next_multiple_of(8), 0);
        }

        fn gen(buf: &mut Vec<u8>) {
            put(buf, XinpGen { xig_len: 24, xig_count: 0, xig_gen: 0, xig_sogen: 0 });
        }

        fn inp(local: SocketAddr, foreign: SocketAddr) -> XinpcbN {
            // SAFETY: Plain; all-zero is a value.
            let mut x: XinpcbN = unsafe { std::mem::zeroed() };
            x.xi_len = 104;
            x.xi_kind = XSO_INPCB;
            x.inp_lport = local.port().to_be();
            x.inp_fport = foreign.port().to_be();
            let bytes = |a: SocketAddr| match a.ip() {
                IpAddr::V4(v4) => {
                    let mut b = [0u8; 16];
                    b[12..].copy_from_slice(&v4.octets());
                    b
                }
                IpAddr::V6(v6) => v6.octets(),
            };
            x.inp_vflag = if local.is_ipv4() { INP_IPV4 } else { INP_IPV6 };
            x.inp_dependladdr = bytes(local);
            x.inp_dependfaddr = bytes(foreign);
            x
        }

        fn so(sock: Sock) -> XsocketN {
            // SAFETY: Plain; all-zero is a value.
            let mut x: XsocketN = unsafe { std::mem::zeroed() };
            x.xso_len = 104;
            x.xso_kind = XSO_SOCKET;
            match sock {
                Sock::Kernel(uid) => {
                    x.xso_so = 0x5eed_0000_0000_0001;
                    x.xso_protocol = 6;
                    x.so_uid = uid;
                }
                Sock::Absent => {}
                Sock::Userland => x.xso_protocol = 6,
            }
            x
        }

        fn tcp(state: i32) -> XtcpcbN {
            // SAFETY: Plain; all-zero is a value.
            let mut x: XtcpcbN = unsafe { std::mem::zeroed() };
            x.xt_len = 204;
            x.xt_kind = XSO_TCPCB;
            x.t_state = state;
            x
        }

        /// One connection's six records, in the kernel's order.
        fn conn(buf: &mut Vec<u8>, local: SocketAddr, foreign: SocketAddr, state: i32, sock: Sock) {
            put(buf, inp(local, foreign));
            put(buf, so(sock));
            skipped(buf, 32, 0x002);
            skipped(buf, 32, 0x004);
            skipped(buf, 136, 0x008);
            put(buf, tcp(state));
        }

        /// A table: header, `conns`, closing header.
        fn table(conns: &[(SocketAddr, SocketAddr, i32, Sock)]) -> Vec<u8> {
            let mut buf = Vec::new();
            gen(&mut buf);
            for &(l, f, s, k) in conns {
                conn(&mut buf, l, f, s, k);
            }
            gen(&mut buf);
            buf
        }

        fn lo(port: u16) -> SocketAddr {
            SocketAddr::from(([127, 0, 0, 1], port))
        }

        const ME: &str = "uid:501";
        const TIME_WAIT: i32 = 10;

        #[test]
        fn the_peers_own_established_connection_names_its_creator() {
            let (listener, peer) = (lo(8080), lo(50000));
            // The accepted end (local = listener) always carries the listener's uid; only the peer's end decides.
            let mine = table(&[
                (listener, peer, TCPS_ESTABLISHED, Sock::Kernel(501)),
                (peer, listener, TCPS_ESTABLISHED, Sock::Kernel(501)),
            ]);
            assert_eq!(owner(&mine, peer, listener, ME), PeerOwner::Mine);
            let theirs = table(&[
                (listener, peer, TCPS_ESTABLISHED, Sock::Kernel(501)),
                (peer, listener, TCPS_ESTABLISHED, Sock::Kernel(502)),
            ]);
            assert_eq!(owner(&theirs, peer, listener, ME), PeerOwner::Other("uid:502".into()));
            // A client on an IPv6 socket connecting to ::ffff:127.0.0.1 is stored as IPv4; the accepted address
            // may come mapped.
            let mapped = SocketAddr::from((std::net::Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped(), 50000));
            assert_eq!(owner(&mine, mapped, listener, ME), PeerOwner::Mine);
            // IPv6 loopback.
            let (l6, p6) = ("[::1]:8080".parse().unwrap(), "[::1]:50000".parse().unwrap());
            assert_eq!(owner(&table(&[(p6, l6, TCPS_ESTABLISHED, Sock::Kernel(501))]), p6, l6, ME), PeerOwner::Mine);
            // No connection from the peer.
            assert!(matches!(owner(&mine, lo(50001), listener, ME), PeerOwner::Unknown(_)));
        }

        #[test]
        fn a_closing_socketless_or_user_space_connection_names_no_owner() {
            let (listener, peer) = (lo(8080), lo(50000));
            for (state, sock, own) in [
                (TIME_WAIT, Sock::Kernel(501), ME),
                (TCPS_ESTABLISHED, Sock::Absent, "uid:0"),
                (TCPS_ESTABLISHED, Sock::Userland, "uid:0"),
            ] {
                let t = table(&[(peer, listener, state, sock)]);
                assert!(matches!(owner(&t, peer, listener, own), PeerOwner::Unknown(_)), "state {state}");
            }
        }

        #[test]
        fn a_record_of_another_length_refuses() {
            let (listener, peer) = (lo(8080), lo(50000));
            let good = table(&[(peer, listener, TCPS_ESTABLISHED, Sock::Kernel(501))]);
            assert_eq!(owner(&good, peer, listener, ME), PeerOwner::Mine);
            // The socket record (at 24 + 104) one u32 longer, as a kernel that grew xsocket_n would write it.
            let mut longer = good[..128].to_vec();
            let mut s = good[128..232].to_vec();
            s[..4].copy_from_slice(&108u32.to_ne_bytes());
            s.extend_from_slice(&[0; 4]);
            longer.extend_from_slice(&s);
            longer.resize(longer.len().next_multiple_of(8), 0);
            longer.extend_from_slice(&good[232..]);
            assert!(matches!(owner(&longer, peer, listener, ME), PeerOwner::Unknown(e) if e.contains("108")));
            // A header of another length.
            let mut head = good.clone();
            head[..4].copy_from_slice(&32u32.to_ne_bytes());
            assert!(matches!(owner(&head, peer, listener, ME), PeerOwner::Unknown(_)));
        }

        #[test]
        fn a_truncated_or_disordered_table_refuses() {
            let (listener, peer) = (lo(8080), lo(50000));
            let good = table(&[(peer, listener, TCPS_ESTABLISHED, Sock::Kernel(501))]);
            for cut in [0, 4, 24, 100, good.len() - 30, good.len() - 24, good.len() - 1] {
                assert!(matches!(owner(&good[..cut], peer, listener, ME), PeerOwner::Unknown(_)), "cut {cut}");
            }
            let mut extra = good.clone();
            extra.extend_from_slice(&[0; 8]);
            assert!(matches!(owner(&extra, peer, listener, ME), PeerOwner::Unknown(_)));
            // A connection with no socket record.
            let mut missing = Vec::new();
            gen(&mut missing);
            put(&mut missing, inp(peer, listener));
            put(&mut missing, tcp(TCPS_ESTABLISHED));
            gen(&mut missing);
            assert!(matches!(owner(&missing, peer, listener, ME), PeerOwner::Unknown(_)));
            // A socket record before any connection record.
            let mut early = Vec::new();
            gen(&mut early);
            put(&mut early, so(Sock::Kernel(501)));
            early.extend_from_slice(&good[24..]);
            assert!(matches!(owner(&early, peer, listener, ME), PeerOwner::Unknown(_)));
        }
    }
}

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
