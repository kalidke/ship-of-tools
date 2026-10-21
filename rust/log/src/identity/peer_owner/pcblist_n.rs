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
#[path = "pcblist_n_tests.rs"]
mod tests;
