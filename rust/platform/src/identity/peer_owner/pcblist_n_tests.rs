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
