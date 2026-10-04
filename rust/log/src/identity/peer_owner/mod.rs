//! Whose OS account is on the far end of a loopback TCP connection this process accepted. Decision 0031: every
//! browser-facing port Ship of Tools opens serves only the OS account that opened it. One lookup per accepted
//! connection, on a blocking thread:
//! - Linux: the kernel's TCP table (`/proc/net/tcp`, then `/proc/net/tcp6` for a client on a dual-stack socket);
//!   the peer's own row carries its uid.
//! - Windows: the owner-pid TCP table (`GetExtendedTcpTable`, IPv4 then IPv6 with v4-mapped addresses), then that
//!   process's token user SID.
//! - macOS: no table carries an owner, so this account's own processes are searched for the peer's socket
//!   (libproc) within `MACOS_BUDGET`; not found, or over budget, is a refusal.
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
    /// The lookup could not decide (no row, a closed peer, an API error, macOS over budget): refused.
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
#[cfg(any(target_os = "linux", test))]
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
/// end of the connection), if there is one.
#[cfg(any(target_os = "linux", test))]
fn uid_of_row(table: &str, peer: SocketAddr, listener: SocketAddr) -> Option<u32> {
    table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        if parse_endpoint(f.get(1)?)? == peer && parse_endpoint(f.get(2)?)? == listener {
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

#[cfg(target_os = "macos")]
mod imp {
    use super::PeerOwner;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::{Duration, Instant};

    /// A lookup that takes longer than this refuses the connection (decision 0031: on macOS, if the owner scan
    /// costs too much, refuse rather than serve unchecked).
    pub(super) const MACOS_BUDGET: Duration = Duration::from_millis(200);
    // <sys/proc_info.h> values the libc crate does not carry.
    const PROC_UID_ONLY: u32 = 4;
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    const SOCKINFO_TCP: i32 = 2;
    const INI_IPV4: u8 = 0x1;

    // Layouts transcribed from <sys/proc_info.h>. The assertions below pin the arithmetic; `socket_info` also checks
    // the kernel's byte count, so a layout this macOS does not have is refused, never misread.
    #[allow(dead_code)]
    #[repr(C)]
    struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: i64,
        fi_type: i32,
        fi_guardflags: u32,
    }
    #[allow(dead_code)]
    #[repr(C)]
    struct VinfoStat {
        vst_dev: u32,
        vst_mode: u16,
        vst_nlink: u16,
        vst_ino: u64,
        vst_uid: u32,
        vst_gid: u32,
        vst_times: [i64; 8],
        vst_size: i64,
        vst_blocks: i64,
        vst_blksize: i32,
        vst_flags: u32,
        vst_gen: u32,
        vst_rdev: u32,
        vst_qspare: [i64; 2],
    }
    #[allow(dead_code)]
    #[repr(C)]
    struct SockbufInfo {
        sbi_cc: u32,
        sbi_hiwat: u32,
        sbi_mbcnt: u32,
        sbi_mbmax: u32,
        sbi_lowat: u32,
        sbi_flags: i16,
        sbi_timeo: i16,
    }
    #[allow(dead_code)]
    #[repr(C)]
    struct In4In6Addr {
        i46a_pad32: [u32; 3],
        i46a_addr4: [u8; 4],
    }
    #[allow(dead_code)]
    #[repr(C)]
    struct InSockInfoV6 {
        in6_hlim: u8,
        in6_cksum: i32,
        in6_ifindex: u16,
        in6_hops: i16,
    }
    #[allow(dead_code)]
    #[repr(C)]
    struct InSockInfo {
        insi_fport: i32,
        insi_lport: i32,
        insi_gencnt: u64,
        insi_flags: u32,
        insi_flow: u32,
        insi_vflag: u8,
        insi_ip_ttl: u8,
        rfu_1: u32,
        insi_faddr: In4In6Addr,
        insi_laddr: In4In6Addr,
        insi_v4_tos: u8,
        insi_v6: InSockInfoV6,
    }
    #[allow(dead_code)]
    #[repr(C)]
    struct TcpSockInfo {
        tcpsi_ini: InSockInfo,
        tcpsi_state: i32,
        tcpsi_timer: [i32; 4],
        tcpsi_mss: i32,
        tcpsi_flags: u32,
        rfu_1: u32,
        tcpsi_tp: u64,
    }
    /// `socket_info.soi_proto`: a union whose largest member (`un_sockinfo`) is 528 bytes; only the TCP arm is read.
    #[allow(dead_code)]
    #[repr(C)]
    struct SoiProto {
        pri_tcp: TcpSockInfo,
        _rest: [u8; 528 - 120],
    }
    #[allow(dead_code)]
    #[repr(C)]
    struct SocketInfo {
        soi_stat: VinfoStat,
        soi_so: u64,
        soi_pcb: u64,
        soi_type: i32,
        soi_protocol: i32,
        soi_family: i32,
        soi_options: i16,
        soi_linger: i16,
        soi_state: i16,
        soi_qlen: i16,
        soi_incqlen: i16,
        soi_qlimit: i16,
        soi_timeo: i16,
        soi_error: u16,
        soi_oobmark: u32,
        soi_rcv: SockbufInfo,
        soi_snd: SockbufInfo,
        soi_kind: i32,
        rfu_1: u32,
        soi_proto: SoiProto,
    }
    #[allow(dead_code)]
    #[repr(C)]
    struct SocketFdInfo {
        pfi: ProcFileInfo,
        psi: SocketInfo,
    }

    const _: () = {
        use std::mem::{offset_of, size_of};
        assert!(size_of::<ProcFileInfo>() == 24);
        assert!(size_of::<VinfoStat>() == 136);
        assert!(size_of::<InSockInfo>() == 80);
        assert!(offset_of!(InSockInfo, insi_faddr) == 32);
        assert!(offset_of!(InSockInfo, insi_laddr) == 48);
        assert!(size_of::<TcpSockInfo>() == 120);
        assert!(offset_of!(SocketInfo, soi_kind) == 232);
        assert!(offset_of!(SocketInfo, soi_proto) == 240);
        assert!(size_of::<SocketFdInfo>() == 792);
    };

    /// This uid's process ids.
    fn pids() -> Vec<i32> {
        let uid = unsafe { libc::geteuid() };
        let bytes = unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
        if bytes <= 0 {
            return Vec::new();
        }
        let mut buf = vec![0i32; bytes as usize / 4 + 32];
        let ret = unsafe {
            libc::proc_listpids(PROC_UID_ONLY, uid, buf.as_mut_ptr().cast(), (buf.len() * 4) as libc::c_int)
        };
        if ret <= 0 {
            return Vec::new();
        }
        buf.truncate(ret as usize / 4);
        buf.retain(|&p| p != 0);
        buf
    }

    /// The socket file descriptors of `pid`.
    fn socket_fds(pid: i32) -> Vec<i32> {
        let size = std::mem::size_of::<libc::proc_fdinfo>();
        let bytes = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if bytes <= 0 {
            return Vec::new();
        }
        let mut fds: Vec<libc::proc_fdinfo> = Vec::new();
        fds.resize_with(bytes as usize / size + 16, || libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 });
        let ret = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), (fds.len() * size) as libc::c_int)
        };
        if ret <= 0 {
            return Vec::new();
        }
        fds.truncate(ret as usize / size);
        fds.iter()
            .filter(|f| f.proc_fdtype == libc::PROX_FDTYPE_SOCKET as u32)
            .map(|f| f.proc_fd)
            .collect()
    }

    /// `Ok(None)`: the fd or process went away. `Err`: the kernel's layout is not the one transcribed here.
    fn socket_info(pid: i32, fd: i32) -> Result<Option<SocketFdInfo>, String> {
        let size = std::mem::size_of::<SocketFdInfo>();
        // SAFETY: every field is an integer or an array of integers, so all-zero is a valid value.
        let mut info: SocketFdInfo = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::proc_pidfdinfo(pid, fd, PROC_PIDFDSOCKETINFO, (&mut info as *mut SocketFdInfo).cast(), size as libc::c_int)
        };
        if ret <= 0 {
            return Ok(None);
        }
        if ret as usize != size {
            return Err(format!("socket_fdinfo is {ret} bytes on this macOS, {size} here"));
        }
        Ok(Some(info))
    }

    /// Whether `info` is a TCP socket held at `held` and connected to `other`.
    fn holds(info: &SocketFdInfo, held: SocketAddrV4, other: SocketAddrV4) -> bool {
        if info.psi.soi_kind != SOCKINFO_TCP {
            return false;
        }
        let ini = &info.psi.soi_proto.pri_tcp.tcpsi_ini;
        ini.insi_vflag & INI_IPV4 != 0
            && Ipv4Addr::from(ini.insi_laddr.i46a_addr4) == *held.ip()
            && u16::from_be(ini.insi_lport as u16) == held.port()
            && Ipv4Addr::from(ini.insi_faddr.i46a_addr4) == *other.ip()
            && u16::from_be(ini.insi_fport as u16) == other.port()
    }

    /// The scan is restricted to this uid's processes, so finding the peer's socket there is the ownership proof.
    pub(super) fn lookup(local: SocketAddr, peer: SocketAddr, _own: &str) -> PeerOwner {
        let (SocketAddr::V4(local), SocketAddr::V4(peer)) = (local, peer) else {
            return PeerOwner::Unknown("IPv6 peer: no macOS lookup".into());
        };
        let deadline = Instant::now() + MACOS_BUDGET;
        for pid in pids() {
            if Instant::now() > deadline {
                return PeerOwner::Unknown(format!("over the {} ms macOS lookup budget", MACOS_BUDGET.as_millis()));
            }
            for fd in socket_fds(pid) {
                match socket_info(pid, fd) {
                    Ok(Some(info)) if holds(&info, peer, local) => return PeerOwner::Mine,
                    Ok(_) => {}
                    Err(e) => return PeerOwner::Unknown(e),
                }
            }
        }
        PeerOwner::Unknown("no socket of this account holds the peer's address".into())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
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

    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_lookup_fits_its_budget() {
        let (accepted, _client) = accepted_pair();
        let (local, peer) = (accepted.local_addr().unwrap(), accepted.peer_addr().unwrap());
        let mut times: Vec<std::time::Duration> = (0..20)
            .map(|_| {
                let t0 = std::time::Instant::now();
                assert_eq!(tcp_peer_owner(local, peer), PeerOwner::Mine);
                t0.elapsed()
            })
            .collect();
        times.sort();
        let (median, max) = (times[times.len() / 2], times[times.len() - 1]);
        assert!(median < std::time::Duration::from_millis(50), "median {median:?}, maximum {max:?}");
    }
}
