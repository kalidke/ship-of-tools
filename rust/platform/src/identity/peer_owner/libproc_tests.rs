use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};

// Raw os errors are plain numbers: this file builds on Windows too, and only the number matters here.
const EPERM: i32 = 1;
const ESRCH: i32 = 3;

fn v4(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

fn addr_bytes(a: SocketAddr) -> [u8; 16] {
    match a.ip() {
        IpAddr::V4(ip) => {
            let mut b = [0u8; 16];
            b[12..].copy_from_slice(&ip.octets());
            b
        }
        IpAddr::V6(ip) => ip.octets(),
    }
}

fn to_bytes(r: &SocketFdinfo) -> Vec<u8> {
    // SAFETY: `SocketFdinfo` has integer and byte-array fields only and no padding, so every byte is initialized.
    unsafe {
        std::slice::from_raw_parts(
            (r as *const SocketFdinfo).cast::<u8>(),
            size_of::<SocketFdinfo>(),
        )
    }
    .to_vec()
}

/// A kernel record of a TCP socket of kind `kind` in state `state`, `vflag` saying how its addresses are stored.
fn record_with(
    kind: i32,
    state: i32,
    vflag: u8,
    local: SocketAddr,
    foreign: SocketAddr,
) -> SocketFdinfo {
    // SAFETY: integer and byte-array fields only: all-zero is a value.
    let mut r: SocketFdinfo = unsafe { std::mem::zeroed() };
    r.soi_kind = kind;
    r.tcpsi_state = state;
    r.insi_vflag = vflag;
    r.insi_lport = i32::from(local.port().to_be());
    r.insi_fport = i32::from(foreign.port().to_be());
    r.insi_laddr = addr_bytes(local);
    r.insi_faddr = addr_bytes(foreign);
    r
}

/// An established TCP socket held with `local` as its own endpoint, as the kernel stores `local`'s family.
fn record(local: SocketAddr, foreign: SocketAddr) -> Vec<u8> {
    let vflag = if local.is_ipv4() { INI_IPV4 } else { INI_IPV6 };
    to_bytes(&record_with(
        SOCKINFO_TCP,
        TSI_S_ESTABLISHED,
        vflag,
        local,
        foreign,
    ))
}

#[test]
fn only_the_peers_own_established_tcp_end_matches() {
    let (peer, listener) = (v4(50000), v4(8080));
    let own_end = |r: Vec<u8>, peer, listener| peer_end(&r, peer, listener).unwrap();
    // The peer's own end: local is the peer, foreign is the listener.
    assert!(own_end(record(peer, listener), peer, listener));
    // The listener's accepted end, held by this process, has the endpoints the other way round.
    assert!(!own_end(record(listener, peer), peer, listener));
    // An IPv4-mapped accepted address reads as IPv4, on either side.
    let mapped = |a: SocketAddr| SocketAddr::from((Ipv4Addr::LOCALHOST.to_ipv6_mapped(), a.port()));
    assert!(own_end(record(peer, listener), mapped(peer), listener));
    assert!(own_end(record(peer, listener), peer, mapped(listener)));
    // A dual-stack client of an IPv4 listener holds an IPv6 socket whose addresses are the mapped ones.
    assert!(own_end(
        record(mapped(peer), mapped(listener)),
        peer,
        listener
    ));
    assert!(own_end(
        record(mapped(peer), mapped(listener)),
        mapped(peer),
        mapped(listener)
    ));
    // `[::1]` on both ends.
    let (p6, l6) = (
        SocketAddr::from((Ipv6Addr::LOCALHOST, 50001)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, 8081)),
    );
    assert!(own_end(record(p6, l6), p6, l6));
    assert!(!own_end(record(l6, p6), p6, l6));
    // A record of the right endpoints that is not an established TCP socket, or whose storage is unknown.
    let wrong = |kind, state, vflag| to_bytes(&record_with(kind, state, vflag, peer, listener));
    assert!(
        !own_end(wrong(SOCKINFO_TCP, 10, INI_IPV4), peer, listener),
        "state 10: TIME_WAIT"
    );
    assert!(
        !own_end(wrong(SOCKINFO_TCP, 5, INI_IPV4), peer, listener),
        "state 5: CLOSE_WAIT"
    );
    assert!(
        !own_end(wrong(1, TSI_S_ESTABLISHED, INI_IPV4), peer, listener),
        "kind 1: not TCP"
    );
    assert!(
        !own_end(wrong(SOCKINFO_TCP, TSI_S_ESTABLISHED, 0), peer, listener),
        "vflag 0 on IPv4 addresses"
    );
    // With neither bit set the addresses are not stored in either form: not IPv4's last four bytes, not IPv6 whole.
    let none = to_bytes(&record_with(SOCKINFO_TCP, TSI_S_ESTABLISHED, 0, p6, l6));
    assert!(!own_end(none, p6, l6), "vflag 0 on IPv6 addresses");
    // Another port on either endpoint.
    assert!(!own_end(record(peer, listener), v4(50002), listener));
    assert!(!own_end(record(peer, listener), peer, v4(8082)));
}

#[test]
fn a_socket_record_of_another_length_refuses_by_its_length() {
    let (peer, listener) = (v4(50000), v4(8080));
    let whole = record(peer, listener);
    assert_eq!(whole.len(), 792);
    for len in [791, 800] {
        let mut bytes = whole.clone();
        bytes.resize(len, 0);
        let why = peer_end(&bytes, peer, listener).unwrap_err();
        assert!(
            why.contains(&len.to_string()) && why.contains("792"),
            "{why}"
        );
    }
    let mut fake = Fake::of(vec![holder(
        10,
        Ok(501),
        vec![(3, Rec::Bytes(whole[..791].to_vec()))],
    )]);
    let verdict = owner(&fake, listener, peer, "uid:501");
    assert!(
        matches!(&verdict, PeerOwner::Unknown(why) if why.contains("791")),
        "{verdict:?}"
    );
    fake.procs[0].records.insert(3, Rec::TooBig);
    let verdict = owner(&fake, listener, peer, "uid:501");
    assert!(
        matches!(&verdict, PeerOwner::Unknown(why) if why.contains("792")),
        "{verdict:?}"
    );
}

/// What the fake kernel answers for one socket descriptor.
#[derive(Clone)]
enum Rec {
    Bytes(Vec<u8>),
    /// The kernel's record is larger than the buffer (ENOMEM).
    TooBig,
    /// A raw os error.
    Errno(i32),
}

/// One fake process: its effective uid, its socket descriptors, and a record or an error per descriptor.
struct FakeProc {
    pid: i32,
    euid: Result<u32, i32>,
    sockets: Result<Vec<i32>, i32>,
    records: BTreeMap<i32, Rec>,
}

fn holder(pid: i32, euid: Result<u32, i32>, records: Vec<(i32, Rec)>) -> FakeProc {
    FakeProc {
        pid,
        euid,
        sockets: Ok(records.iter().map(|(fd, _)| *fd).collect()),
        records: records.into_iter().collect(),
    }
}

/// The kernel, as a list of processes; it remembers the uid it was asked to list.
struct Fake {
    procs: Vec<FakeProc>,
    pids_fails: Option<i32>,
    asked: RefCell<Vec<u32>>,
}

impl Fake {
    fn of(procs: Vec<FakeProc>) -> Fake {
        Fake {
            procs,
            pids_fails: None,
            asked: RefCell::new(Vec::new()),
        }
    }
    fn proc(&self, pid: i32) -> io::Result<&FakeProc> {
        self.procs
            .iter()
            .find(|p| p.pid == pid)
            .ok_or_else(|| io::Error::from_raw_os_error(ESRCH))
    }
}

impl Procs for Fake {
    fn pids(&self, euid: u32) -> io::Result<Vec<i32>> {
        self.asked.borrow_mut().push(euid);
        match self.pids_fails {
            Some(code) => Err(io::Error::from_raw_os_error(code)),
            None => Ok(self.procs.iter().map(|p| p.pid).collect()),
        }
    }
    fn sockets(&self, pid: i32) -> io::Result<Vec<i32>> {
        self.proc(pid)?
            .sockets
            .clone()
            .map_err(io::Error::from_raw_os_error)
    }
    fn socket(&self, pid: i32, fd: i32) -> io::Result<Vec<u8>> {
        match self.proc(pid)?.records.get(&fd) {
            Some(Rec::Bytes(b)) => Ok(b.clone()),
            Some(Rec::TooBig) => Err(io::ErrorKind::OutOfMemory.into()),
            Some(Rec::Errno(code)) => Err(io::Error::from_raw_os_error(*code)),
            None => Err(io::Error::from_raw_os_error(ESRCH)),
        }
    }
    fn euid(&self, pid: i32) -> io::Result<u32> {
        self.proc(pid)?.euid.map_err(io::Error::from_raw_os_error)
    }
}

#[test]
fn the_holders_effective_uid_decides_and_an_unread_process_never_counts() {
    let (peer, listener) = (v4(50000), v4(8080));
    let theirs = || (4, Rec::Bytes(record(peer, listener)));
    let accepted = || (3, Rec::Bytes(record(listener, peer)));
    let judge = |fake: &Fake| owner(fake, listener, peer, "uid:501");

    // The holder's euid is own's, or another's.
    assert_eq!(
        judge(&Fake::of(vec![holder(
            10,
            Ok(501),
            vec![accepted(), theirs()]
        )])),
        PeerOwner::Mine
    );
    assert_eq!(
        judge(&Fake::of(vec![holder(10, Ok(502), vec![theirs()])])),
        PeerOwner::Other("uid:502".into())
    );
    // The first holder found decides.
    let two = |first, second| {
        Fake::of(vec![
            holder(10, Ok(first), vec![theirs()]),
            holder(11, Ok(second), vec![theirs()]),
        ])
    };
    assert_eq!(judge(&two(501, 502)), PeerOwner::Mine);
    assert_eq!(judge(&two(502, 501)), PeerOwner::Other("uid:502".into()));

    // The only holder cannot be read: a process or a socket the walk could not read never counts.
    let mut unreadable = holder(10, Ok(501), vec![theirs()]);
    unreadable.sockets = Err(EPERM);
    assert!(matches!(
        judge(&Fake::of(vec![unreadable])),
        PeerOwner::Unknown(_)
    ));
    let unreadable = holder(10, Ok(501), vec![(4, Rec::Errno(EPERM))]);
    assert!(matches!(
        judge(&Fake::of(vec![unreadable])),
        PeerOwner::Unknown(_)
    ));
    // A process that exited first is skipped and the holder after it is found.
    let mut gone = holder(9, Ok(501), vec![]);
    gone.sockets = Err(ESRCH);
    assert_eq!(
        judge(&Fake::of(vec![gone, holder(10, Ok(501), vec![theirs()])])),
        PeerOwner::Mine
    );
    // The holder's uid cannot be read.
    assert!(matches!(
        judge(&Fake::of(vec![holder(10, Err(ESRCH), vec![theirs()])])),
        PeerOwner::Unknown(_)
    ));
    // This process's own accepted end is not the peer's.
    assert!(matches!(
        judge(&Fake::of(vec![holder(10, Ok(501), vec![accepted()])])),
        PeerOwner::Unknown(_)
    ));

    // The list of processes cannot be read.
    let mut no_list = Fake::of(vec![holder(10, Ok(501), vec![theirs()])]);
    no_list.pids_fails = Some(EPERM);
    assert!(matches!(judge(&no_list), PeerOwner::Unknown(_)));

    // An account that is not a uid is refused before the kernel is asked anything.
    let fake = Fake::of(vec![holder(10, Ok(501), vec![theirs()])]);
    for own in ["S-1-5-21-1", "uid:", "uid:-1", "501"] {
        assert!(
            matches!(owner(&fake, listener, peer, own), PeerOwner::Unknown(_)),
            "{own}"
        );
    }
    assert!(fake.asked.borrow().is_empty());
    // The processes asked for are this account's.
    judge(&fake);
    assert_eq!(*fake.asked.borrow(), [501]);

    // With no holder the reason counts what was read and what was not.
    let fake = Fake::of(vec![
        holder(10, Ok(501), vec![accepted()]),
        holder(11, Ok(501), vec![(3, Rec::Bytes(record(v4(1), v4(2))))]),
        holder(12, Ok(501), vec![]),
        FakeProc {
            sockets: Err(EPERM),
            ..holder(13, Ok(501), vec![])
        },
    ]);
    let PeerOwner::Unknown(why) = judge(&fake) else {
        panic!("a holder was found")
    };
    assert!(why.contains("4 processes, 2 sockets read"), "{why}");
    assert!(why.contains("1: 1"), "{why}");
}

#[test]
fn a_list_the_kernel_fills_to_its_last_entry_is_read_again() {
    // Entries of 4 bytes; the kernel's hint says 16 bytes, so 4 entries.
    let hint = || Ok(16usize);
    let capacities = RefCell::new(Vec::new());
    // Writes `n` entries 0.. into the buffer and says how many bytes that is, as the kernel does.
    let fills = |n: usize, buf: &mut [u32]| {
        capacities.borrow_mut().push(buf.len());
        let n = n.min(buf.len());
        for (i, x) in buf.iter_mut().take(n).enumerate() {
            *x = i as u32 + 1;
        }
        Ok(n * 4)
    };

    // A short fill is returned whole.
    assert_eq!(read_list(0u32, hint, |b| fills(2, b)).unwrap(), [1, 2]);
    assert_eq!(*capacities.borrow(), [4]);

    // A full buffer is read again, into twice the room.
    capacities.borrow_mut().clear();
    let mut call = 0;
    let got = read_list(0u32, hint, |b| {
        call += 1;
        fills(if call == 1 { 4 } else { 6 }, b)
    })
    .unwrap();
    assert_eq!(got, [1, 2, 3, 4, 5, 6]);
    assert_eq!(*capacities.borrow(), [4, 8]);

    // A list that keeps filling refuses, after three reads.
    capacities.borrow_mut().clear();
    let err = read_list(0u32, hint, |b| fills(usize::MAX, b)).unwrap_err();
    assert!(err.to_string().contains("kept growing"), "{err}");
    assert_eq!(*capacities.borrow(), [4, 8, 16]);

    // A hint that says nothing still reads, into one entry.
    capacities.borrow_mut().clear();
    assert_eq!(
        read_list(0u32, || Ok(0), |b| fills(0, b)).unwrap(),
        Vec::<u32>::new()
    );
    assert_eq!(*capacities.borrow(), [1]);

    // A failed hint or a failed fill is the answer.
    let err = read_list(
        0u32,
        || Err(io::Error::from_raw_os_error(EPERM)),
        |b| fills(0, b),
    )
    .unwrap_err();
    assert_eq!(err.raw_os_error(), Some(EPERM));
    let err = read_list(0u32, hint, |_| Err(io::Error::from_raw_os_error(ESRCH))).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(ESRCH));
}

/// What the real libproc answers about a process and a socket this test holds. The counts it prints are what the
/// cost bound in the contract is read against.
#[cfg(target_os = "macos")]
#[test]
fn libproc_reads_this_process_and_its_own_client_socket() {
    use std::os::fd::AsRawFd;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (accepted, _) = listener.accept().unwrap();
    let (listen_addr, client_addr) = (listener.local_addr().unwrap(), client.local_addr().unwrap());
    // SAFETY: geteuid has no arguments and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let me = i32::try_from(std::process::id()).unwrap();

    let pids = Libproc.pids(euid).unwrap();
    assert!(
        pids.contains(&me),
        "this process is not among the {} pids of its own uid",
        pids.len()
    );
    let sockets = Libproc.sockets(me).unwrap();
    assert!(
        sockets.contains(&client.as_raw_fd()),
        "the client's descriptor is not among {sockets:?}"
    );

    let mine = Libproc.socket(me, client.as_raw_fd()).unwrap();
    assert_eq!(mine.len(), 792);
    assert!(
        peer_end(&mine, client_addr, listen_addr).unwrap(),
        "the client's own record is not the peer's end"
    );
    let theirs = Libproc.socket(me, accepted.as_raw_fd()).unwrap();
    assert!(
        !peer_end(&theirs, client_addr, listen_addr).unwrap(),
        "the accepted end matched"
    );
    assert_eq!(Libproc.euid(me).unwrap(), euid);

    let held: usize = pids
        .iter()
        .filter_map(|&p| Libproc.sockets(p).ok())
        .map(|s| s.len())
        .sum();
    println!(
        "libproc: {} processes of this account, {held} socket descriptors",
        pids.len()
    );
}
