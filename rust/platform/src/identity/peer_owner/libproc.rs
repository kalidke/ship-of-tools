//! The macOS owner lookup: walk this account's processes through libproc, find the process that holds the peer's own
//! end of the connection, and name that process's effective uid. On macOS 27.0.1 the kernel TCP table
//! (`net.inet.tcp.pcblist_n`) holds no record of another process's connection (measured), and `socket_fdinfo`'s
//! `soi_stat`, which could carry the creator, is not filled in the macOS 13 and 26 sources, so the rule is the
//! holder's account, not the creator's. The first holder found decides.
//!
//! The portable part (the record, its decode, the walk over [`Procs`], the list reader) comes first and is compiled for
//! tests on every platform; the libproc calls that answer [`Procs`] are macOS's.
use super::{verdict, PeerOwner};
use std::collections::BTreeMap;
use std::io;
use std::mem::{offset_of, size_of};
use std::net::{IpAddr, SocketAddr};

// <sys/proc_info.h>.
const SOCKINFO_TCP: i32 = 2;
const TSI_S_ESTABLISHED: i32 = 4;
const INI_IPV4: u8 = 0x1;
const INI_IPV6: u8 = 0x2;

/// struct socket_fdinfo (<sys/proc_info.h>; identical in xnu-8792, macOS 13, and xnu-12377, macOS 26; 792 bytes
/// measured on macOS 27.0.1): a proc_fileinfo, then a socket_info whose protocol union holds a tcp_sockinfo when
/// soi_kind is SOCKINFO_TCP. Only what is read is transcribed and the rest is named byte arrays, with no implicit
/// padding, so any 792 bytes are a value.
#[repr(C)]
struct SocketFdinfo {
    /// proc_fileinfo (24), then socket_info's soi_stat (136), soi_so, soi_pcb, soi_type, soi_protocol, soi_family,
    /// eight shorts, soi_oobmark, soi_rcv and soi_snd.
    _head: [u8; 256],
    soi_kind: i32,
    _rfu_1: u32,
    // soi_proto.pri_tcp.tcpsi_ini, a struct in_sockinfo. Ports are network-order u_shorts stored in an int.
    insi_fport: i32,
    insi_lport: i32,
    /// insi_gencnt, insi_flags, insi_flow.
    _insi_gencnt_flags_flow: [u8; 16],
    insi_vflag: u8,
    /// insi_ip_ttl, two bytes of C padding, rfu_1.
    _insi_ttl_pad_rfu: [u8; 7],
    /// An in6_addr, or an in4in6_addr whose last four bytes are the IPv4 address.
    insi_faddr: [u8; 16],
    insi_laddr: [u8; 16],
    /// insi_v4, insi_v6.
    _insi_v4_v6: [u8; 16],
    tcpsi_state: i32,
    /// tcpsi_timer through the end of the protocol union.
    _rest: [u8; 444],
}

const _: () = {
    assert!(size_of::<SocketFdinfo>() == 792);
    assert!(offset_of!(SocketFdinfo, soi_kind) == 256);
    assert!(offset_of!(SocketFdinfo, insi_fport) == 264);
    assert!(offset_of!(SocketFdinfo, insi_lport) == 268);
    assert!(offset_of!(SocketFdinfo, insi_vflag) == 288);
    assert!(offset_of!(SocketFdinfo, insi_faddr) == 296);
    assert!(offset_of!(SocketFdinfo, insi_laddr) == 312);
    assert!(offset_of!(SocketFdinfo, tcpsi_state) == 344);
};

/// An endpoint as `insi_vflag` says it is stored; a v4-mapped IPv6 address reads as IPv4.
fn endpoint(vflag: u8, addr: [u8; 16], port: i32) -> Option<(IpAddr, u16)> {
    let ip = if vflag & INI_IPV4 != 0 {
        IpAddr::from([addr[12], addr[13], addr[14], addr[15]])
    } else if vflag & INI_IPV6 != 0 {
        IpAddr::from(addr)
    } else {
        return None;
    };
    Some((ip.to_canonical(), u16::from_be(port as u16)))
}

/// Whether `record`, a socket's `socket_fdinfo`, is the peer's own end of the connection `listener` accepted from
/// `peer`: an established TCP socket whose local endpoint is `peer` and foreign endpoint is `listener`. The
/// listener's accepted end has the two the other way round and never matches. It names no uid. A record of any
/// other length is a layout this code does not know, refused by its length.
pub(super) fn peer_end(
    record: &[u8],
    peer: SocketAddr,
    listener: SocketAddr,
) -> Result<bool, String> {
    if record.len() != size_of::<SocketFdinfo>() {
        return Err(format!(
            "socket_fdinfo is {} bytes on this macOS, {} here",
            record.len(),
            size_of::<SocketFdinfo>()
        ));
    }
    // SAFETY: `record` holds `size_of::<SocketFdinfo>()` bytes, and the type has integer and byte-array fields only
    // with no padding, so any bytes are a value; the read is unaligned by design.
    let r = unsafe { std::ptr::read_unaligned(record.as_ptr().cast::<SocketFdinfo>()) };
    let (peer, listener) = (
        (peer.ip().to_canonical(), peer.port()),
        (listener.ip().to_canonical(), listener.port()),
    );
    Ok(r.soi_kind == SOCKINFO_TCP
        && r.tcpsi_state == TSI_S_ESTABLISHED
        && endpoint(r.insi_vflag, r.insi_laddr, r.insi_lport) == Some(peer)
        && endpoint(r.insi_vflag, r.insi_faddr, r.insi_fport) == Some(listener))
}

/// What the walk asks of the kernel. libproc answers it on macOS; the tests fake it.
pub(super) trait Procs {
    /// The pids of the processes whose effective uid is `euid`, the whole list.
    fn pids(&self, euid: u32) -> io::Result<Vec<i32>>;
    /// The descriptor numbers of `pid`'s sockets (empty for a process with none).
    fn sockets(&self, pid: i32) -> io::Result<Vec<i32>>;
    /// The bytes the kernel wrote for `pid`'s socket `fd`, as many as it returned; an error of kind OutOfMemory
    /// when the kernel's record is larger than the 792 bytes this build reads.
    fn socket(&self, pid: i32, fd: i32) -> io::Result<Vec<u8>>;
    /// The effective uid of the process `pid` names now.
    fn euid(&self, pid: i32) -> io::Result<u32>;
}

/// Whose account holds the peer's end of the connection accepted at `local` (the listener side) from `peer`: the
/// arguments are in `imp::lookup`'s order, and this is where they become the peer's own end's endpoints. `own` is
/// `own_account_id`'s `uid:<n>`. Only the effective uid of the holder, read after its socket matched, ever makes
/// [`PeerOwner::Mine`]: a process or socket the walk could not read counts for nothing. The first holder found
/// decides. A process or socket that fails to read is skipped and counted; a record of an unknown length, or a list
/// of processes that cannot be read, refuses the lookup.
pub(super) fn owner(
    procs: &impl Procs,
    local: SocketAddr,
    peer: SocketAddr,
    own: &str,
) -> PeerOwner {
    let Some(euid) = own.strip_prefix("uid:").and_then(|n| n.parse::<u32>().ok()) else {
        return PeerOwner::Unknown(format!("this process's account {own:?} is not a uid"));
    };
    let pids = match procs.pids(euid) {
        Ok(pids) => pids,
        Err(e) => return PeerOwner::Unknown(format!("listing this account's processes: {e}")),
    };
    let mut sockets_read = 0usize;
    // How many processes and sockets could not be read, by raw os error (-1 for none).
    let mut unread: BTreeMap<i32, usize> = BTreeMap::new();
    let mut skip = |e: io::Error| *unread.entry(e.raw_os_error().unwrap_or(-1)).or_default() += 1;
    for &pid in &pids {
        let fds = match procs.sockets(pid) {
            Ok(fds) => fds,
            Err(e) => {
                skip(e);
                continue;
            }
        };
        for fd in fds {
            let record = match procs.socket(pid, fd) {
                Ok(record) => record,
                Err(e) if e.kind() == io::ErrorKind::OutOfMemory => {
                    return PeerOwner::Unknown(format!(
                        "socket_fdinfo is larger on this macOS than the {} bytes this build reads",
                        size_of::<SocketFdinfo>()
                    ));
                }
                Err(e) => {
                    skip(e);
                    continue;
                }
            };
            sockets_read += 1;
            match peer_end(&record, peer, local) {
                Err(why) => return PeerOwner::Unknown(why),
                Ok(false) => {}
                Ok(true) => {
                    return match procs.euid(pid) {
                        Ok(uid) => verdict(&format!("uid:{uid}"), own),
                        Err(e) => PeerOwner::Unknown(format!(
                            "the uid of process {pid}, which holds the peer's end: {e}"
                        )),
                    };
                }
            }
        }
    }
    PeerOwner::Unknown(format!(
        "no process of this account holds the peer's end: {} processes, {sockets_read} sockets read; not read, by os error: {unread:?}",
        pids.len()
    ))
}

/// A list the kernel writes into a buffer and answers by its byte count, read until it fits. `hint` is the kernel's
/// size for the list now and `fill` writes into a buffer and returns the bytes it wrote. A kernel that fills a buffer
/// to its last entry does not say whether it cut the list short, so a full buffer is read again into more room, three
/// attempts in all; a list that fills every one is refused.
fn read_list<T: Copy>(
    zero: T,
    mut hint: impl FnMut() -> io::Result<usize>,
    mut fill: impl FnMut(&mut [T]) -> io::Result<usize>,
) -> io::Result<Vec<T>> {
    let mut capacity = 0;
    for _ in 0..3 {
        capacity = (hint()? / size_of::<T>()).max(capacity * 2).max(1);
        let mut buf = vec![zero; capacity];
        let entries = fill(&mut buf)? / size_of::<T>();
        if entries < capacity {
            buf.truncate(entries);
            return Ok(buf);
        }
    }
    Err(io::Error::other("the list kept growing"))
}

#[cfg(target_os = "macos")]
pub(super) use macos::Libproc;

#[cfg(target_os = "macos")]
mod macos {
    use super::{read_list, Procs, SocketFdinfo};
    use std::ffi::{c_int, c_void};
    use std::io;
    use std::mem::{offset_of, size_of};

    // <sys/proc_info.h>. libc has neither.
    const PROC_UID_ONLY: u32 = 4;
    const PROC_PIDFDSOCKETINFO: c_int = 3;

    const _: () = {
        assert!(size_of::<libc::proc_fdinfo>() == 8);
        assert!(size_of::<libc::proc_bsdinfo>() == 136);
        assert!(offset_of!(libc::proc_bsdinfo, pbi_uid) == 20);
    };

    /// libproc, the interface `lsof` and Activity Monitor read. It answers 0 on failure with errno set.
    pub(in super::super) struct Libproc;

    /// The byte length of `buf` as libproc's `int`.
    fn byte_len<T>(buf: &[T]) -> io::Result<c_int> {
        c_int::try_from(std::mem::size_of_val(buf))
            .map_err(|_| io::Error::other("a list too large for libproc"))
    }

    /// A libproc answer: its byte count, or the error it set when it answered 0.
    fn counted(n: c_int) -> io::Result<usize> {
        if n > 0 {
            Ok(n as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    impl Procs for Libproc {
        fn pids(&self, euid: u32) -> io::Result<Vec<i32>> {
            read_list(
                0i32,
                // SAFETY: a null buffer of size 0 asks only for the size in bytes.
                || {
                    counted(unsafe {
                        libc::proc_listpids(PROC_UID_ONLY, euid, std::ptr::null_mut(), 0)
                    })
                },
                // SAFETY: `buf` has `byte_len(buf)` writable bytes and the kernel writes at most that many.
                |buf| {
                    counted(unsafe {
                        libc::proc_listpids(
                            PROC_UID_ONLY,
                            euid,
                            buf.as_mut_ptr().cast::<c_void>(),
                            byte_len(buf)?,
                        )
                    })
                },
            )
        }

        fn sockets(&self, pid: i32) -> io::Result<Vec<i32>> {
            let fds = read_list(
                libc::proc_fdinfo {
                    proc_fd: 0,
                    proc_fdtype: 0,
                },
                // SAFETY: a null buffer of size 0 asks only for the size in bytes.
                || {
                    counted(unsafe {
                        libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0)
                    })
                },
                // 0 is a failure or a process with no descriptors, and neither has a socket to read here.
                // SAFETY: `buf` has `byte_len(buf)` writable bytes and the kernel writes at most that many.
                |buf| {
                    let n = unsafe {
                        libc::proc_pidinfo(
                            pid,
                            libc::PROC_PIDLISTFDS,
                            0,
                            buf.as_mut_ptr().cast::<c_void>(),
                            byte_len(buf)?,
                        )
                    };
                    Ok(n.max(0) as usize)
                },
            )?;
            Ok(fds
                .into_iter()
                .filter(|f| f.proc_fdtype == libc::PROX_FDTYPE_SOCKET as u32)
                .map(|f| f.proc_fd)
                .collect())
        }

        fn socket(&self, pid: i32, fd: i32) -> io::Result<Vec<u8>> {
            let mut buf = [0u8; size_of::<SocketFdinfo>()];
            // SAFETY: `buf` has 792 writable bytes and the kernel writes at most the size it is given.
            let n = unsafe {
                libc::proc_pidfdinfo(
                    pid,
                    fd,
                    PROC_PIDFDSOCKETINFO,
                    buf.as_mut_ptr().cast::<c_void>(),
                    buf.len() as c_int,
                )
            };
            if n <= 0 {
                let e = io::Error::last_os_error();
                return Err(if e.raw_os_error() == Some(libc::ENOMEM) {
                    io::Error::new(
                        io::ErrorKind::OutOfMemory,
                        "the kernel's socket record is larger than the buffer",
                    )
                } else {
                    e
                });
            }
            Ok(buf[..n as usize].to_vec())
        }

        fn euid(&self, pid: i32) -> io::Result<u32> {
            // SAFETY: integer and character-array fields only: all-zero is a value.
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = size_of::<libc::proc_bsdinfo>() as c_int;
            // SAFETY: `info` has `size` writable bytes and the kernel writes at most that many.
            let n = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&raw mut info).cast::<c_void>(),
                    size,
                )
            };
            match n {
                n if n == size => Ok(info.pbi_uid),
                n if n <= 0 => Err(io::Error::last_os_error()),
                n => Err(io::Error::other(format!(
                    "proc_bsdinfo is {n} bytes on this macOS, {size} here"
                ))),
            }
        }
    }
}

#[cfg(test)]
#[path = "libproc_tests.rs"]
mod tests;
