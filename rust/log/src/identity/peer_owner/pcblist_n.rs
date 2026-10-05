//! The macOS kernel TCP table walk: `net.inet.tcp.pcblist_n`'s six-record sequence per connection and its verdict.
use super::{verdict, PeerOwner};
use std::mem::{offset_of, size_of};
use std::net::{IpAddr, SocketAddr};

// <sys/socketvar.h>, <netinet/in_pcb.h>, <netinet/tcp_fsm.h>.
const XSO_SOCKET: u32 = 0x001;
const XSO_RCVBUF: u32 = 0x002;
const XSO_SNDBUF: u32 = 0x004;
const XSO_STATS: u32 = 0x008;
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

/// The lengths of the two records whose contents are never read (xsockbuf_n, xsockstat_n): only their length
/// and kind are checked, so they need no transcription.
const XSOCKBUF_N_LEN: usize = 32;
const XSOCKSTAT_N_LEN: usize = 136;

/// The next record at `*off`: its length must be `len` and its kind one of `kinds`. Returns where it starts and
/// moves `*off` past it, padding included.
fn expect(buf: &[u8], off: &mut usize, len: usize, kinds: &[u32]) -> Result<usize, String> {
    let start = *off;
    let got = u32_at(buf, start).ok_or("pcblist_n: truncated")? as usize;
    let kind = u32_at(buf, start + 4).ok_or("pcblist_n: truncated")?;
    if got != len {
        return Err(format!("pcblist_n: a record of {got} bytes where {len} is expected"));
    }
    if !kinds.contains(&kind) {
        return Err(format!("pcblist_n: a record of kind {kind:#x} where {kinds:x?} is expected"));
    }
    *off = start.checked_add(len.next_multiple_of(8)).filter(|&n| n <= buf.len()).ok_or("pcblist_n: truncated")?;
    Ok(start)
}

/// One connection's records.
struct Pcb {
    inp: XinpcbN,
    so: XsocketN,
    tcp: XtcpcbN,
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
fn creator(pcb: Pcb, peer: (IpAddr, u16), listener: (IpAddr, u16)) -> Option<u32> {
    let Pcb { inp, so, tcp } = pcb;
    let live = endpoint(inp.inp_vflag, inp.inp_dependladdr, inp.inp_lport) == Some(peer)
        && endpoint(inp.inp_vflag, inp.inp_dependfaddr, inp.inp_fport) == Some(listener)
        && { tcp.t_state } == TCPS_ESTABLISHED
        && { so.xso_so } != 0;
    live.then_some(so.so_uid)
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
    let mut found: Option<u32> = None;
    loop {
        let len = u32_at(buf, off).ok_or("pcblist_n: truncated")? as usize;
        if len == gen {
            // The closing xinpgen, which must end the table.
            if off + gen != buf.len() {
                return Err("pcblist_n: bytes after the closing record".into());
            }
            return Ok(found);
        }
        // A connection is exactly six records, in this order; a socketless one has kind 0 on its buffers.
        let at = expect(buf, &mut off, size_of::<XinpcbN>(), &[XSO_INPCB])?;
        let inp = record(buf, at, size_of::<XinpcbN>())?;
        let at = expect(buf, &mut off, size_of::<XsocketN>(), &[XSO_SOCKET])?;
        let so: XsocketN = record(buf, at, size_of::<XsocketN>())?;
        let (rcv, snd): (&[u32], &[u32]) =
            if { so.xso_so } == 0 { (&[XSO_RCVBUF, 0], &[XSO_SNDBUF, 0]) } else { (&[XSO_RCVBUF], &[XSO_SNDBUF]) };
        expect(buf, &mut off, XSOCKBUF_N_LEN, rcv)?;
        expect(buf, &mut off, XSOCKBUF_N_LEN, snd)?;
        expect(buf, &mut off, XSOCKSTAT_N_LEN, &[XSO_STATS])?;
        let at = expect(buf, &mut off, size_of::<XtcpcbN>(), &[XSO_TCPCB])?;
        let tcp = record(buf, at, size_of::<XtcpcbN>())?;
        found = found.or(creator(Pcb { inp, so, tcp }, peer, listener));
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

    /// A record of any length and kind: zeros after the two.
    fn raw(buf: &mut Vec<u8>, len: u32, kind: u32) {
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
        // The kernel leaves a socketless connection's buffer records without a kind.
        let (r, s) = if matches!(sock, Sock::Absent) { (0, 0) } else { (XSO_RCVBUF, XSO_SNDBUF) };
        raw(buf, 32, r);
        raw(buf, 32, s);
        raw(buf, 136, XSO_STATS);
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

    #[test]
    fn an_unexpected_record_or_sequence_refuses() {
        let (listener, peer) = (lo(8080), lo(50000));
        let good = table(&[(peer, listener, TCPS_ESTABLISHED, Sock::Kernel(501))]);
        assert_eq!(owner(&good, peer, listener, ME), PeerOwner::Mine);
        let unknown = |t: &[u8]| matches!(owner(t, peer, listener, ME), PeerOwner::Unknown(_));
        // Byte offsets in `good`: header 24, inpcb 104, socket 104, rcvbuf 32, sndbuf 32, stats 136, tcp 208.
        let (rcv, closing) = (232, good.len() - 24);
        let mut extra = Vec::new();
        raw(&mut extra, 8, 0x400);
        // (a) an unknown record before the closing header.
        let mut a = good[..closing].to_vec();
        a.extend_from_slice(&extra);
        a.extend_from_slice(&good[closing..]);
        assert!(unknown(&a), "a");
        // (b) the same between the socket and receive-buffer records.
        let mut b = good[..rcv].to_vec();
        b.extend_from_slice(&extra);
        b.extend_from_slice(&good[rcv..]);
        assert!(unknown(&b), "b");
        // (c) a connection without its buffer and stat records.
        let mut c = good[..rcv].to_vec();
        c.extend_from_slice(&good[rcv + 32 + 32 + 136..]);
        assert!(unknown(&c), "c");
        // (d) a receive-buffer record of 40 bytes.
        let mut d = good[..rcv].to_vec();
        raw(&mut d, 40, XSO_RCVBUF);
        d.extend_from_slice(&good[rcv + 32..]);
        assert!(unknown(&d), "d");
        // (e) a kernel socket whose buffer records have no kind.
        let mut e = good.clone();
        e[rcv + 4..rcv + 8].copy_from_slice(&0u32.to_ne_bytes());
        e[rcv + 36..rcv + 40].copy_from_slice(&0u32.to_ne_bytes());
        assert!(unknown(&e), "e");
        // (f) a socketless connection, then the peer's own: real tables hold such rows.
        let f = table(&[
            (lo(1), lo(2), TCPS_ESTABLISHED, Sock::Absent),
            (peer, listener, TCPS_ESTABLISHED, Sock::Kernel(501)),
        ]);
        assert_eq!(owner(&f, peer, listener, ME), PeerOwner::Mine);
    }
}
