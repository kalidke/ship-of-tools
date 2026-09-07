//! Linux half of the same-connection challenge (ADR 0043 decision 8):
//! `SO_PEERCRED` (steps 1-2, one `getsockopt` call) + same-user comparison
//! (step 3), a race-free pin against pid reuse (`SO_PEERPIDFD` where the
//! kernel has it, else `pidfd_open` validated by start time), and the
//! retained-pidfd process handle a proof returns
//! ([`ChallengedProcess`]). Steps 4-5 (the wire half) are shared,
//! platform-neutral logic in `crate::challenge` -- see that module's own
//! doc; [`challenge()`] and [`authenticate_server()`] below call into it
//! rather than reimplementing it. Mirrors `challenge_win.rs` in SHAPE;
//! every Win32 mechanism there has a POSIX/Linux replacement here.
//!
//! Linux only (`#![cfg(target_os = "linux")]`): other Unix has no
//! portable equivalent of `SO_PEERCRED`'s pid field (a peer in a
//! different pid namespace, or a non-Linux kernel, may not report one at
//! all) and no `pidfd_open`/`SO_PEERPIDFD` at all -- it fails closed at
//! `socket_unix::connect_voyage_socket`'s own stub
//! (`SocketError::Unsupported`), never here (ADR 0043 decision 8).
//!
//! # Why `SO_PEERCRED` on the CLIENT's own fd works (verified empirically)
//!
//! `SO_PEERCRED`/`SO_PEERPIDFD` are latched onto a connected `AF_UNIX`
//! stream socket at `connect(2)` time with the CONNECTING process's own
//! credentials -- both the newly-accepted server-side socket AND the
//! client's own socket carry that SAME value. For our usage (the server
//! always calls `socket`/`bind`/`listen`/`accept` itself, in the one
//! process that will run `accept_loop`, never on an fd inherited from a
//! DIFFERENT process) this resolves correctly: the client's own
//! `SO_PEERCRED` reports the REAL server process's pid, exactly like
//! `GetNamedPipeServerProcessId` on Windows resolves the peer "from
//! either end of a named pipe" (`challenge_win.rs`'s own doc). Confirmed
//! on this host (kernel 5.15) with a real two-process `socket`/`bind`/
//! `listen`/`accept` server in a freshly forked child and a `connect`ing
//! parent: the parent's own `SO_PEERCRED.pid` was the child's real pid,
//! not its own. (An artificial variant where the LISTENING socket itself
//! was created by a parent and inherited by a child that then called
//! `accept` on it reported the ORIGINAL creator's pid instead -- that
//! shape never arises in this crate's own usage, where `bind`/`listen`/
//! `accept` always run together in the one real server process, so it is
//! noted here only as the reason this doc calls out "verified
//! empirically" rather than merely citing the man page.)

#![cfg(target_os = "linux")]

use crate::challenge::{
    exchange_identity, ChallengeOutcome, ChallengeableConnection, SidAuthOutcome, SidAuthenticated,
    StatusFailure,
};
use crate::exchange::IdentityExchange;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

/// The Linux-shaped extension every [`ChallengeableConnection`] this
/// crate actually challenges must also supply: raw fd access (for
/// `SO_PEERCRED`/pinning) and the connection's own pre-connect anchor the
/// race-free pin below needs -- the Linux twin of
/// `challenge_win::PipeChallengeable`.
pub trait SocketChallengeable: ChallengeableConnection {
    /// This end's own fd for the connected socket -- `SO_PEERCRED`/
    /// `SO_PEERPIDFD` resolve the peer from either end (see this module's
    /// own doc), so the CLIENT's own fd is exactly what steps 1-3 need.
    fn raw_fd(&self) -> RawFd;
    /// `CLOCK_BOOTTIME`, in the SAME clock ticks as `/proc/<pid>/stat`
    /// field 22, sampled immediately BEFORE this connection's own
    /// `connect(2)` attempt began -- the race-free pin's own anchor
    /// (review round fix: a value sampled AFTER `connect` returns leaves
    /// a window open. A peer that connect()s successfully was
    /// necessarily alive when the attempt BEGAN, so its start time must
    /// be strictly earlier than that pre-attempt instant -- UNLESS it is
    /// a replacement that started in the gap between the anchor sample
    /// and the connect completing, which the pin's strict `<` then
    /// correctly classifies `Undetermined`, never a false `Proven`; a
    /// legitimate peer caught in that narrow a race is proven on a later
    /// retry, which re-anchors). See `socket_unix::connect_unix_socket_unchallenged`
    /// for where this is sampled.
    fn connect_anchor_boot_ticks(&self) -> u64;
}

/// `SO_PEERCRED`'s own `(pid, uid, gid)` -- the Linux twin of
/// `GetNamedPipeServerProcessId` (pid) plus the SID lookup (uid/gid),
/// done as ONE syscall instead of several. A `pid` of 0 means the peer is
/// in a different pid namespace, or credentials are otherwise
/// unavailable -- `Undetermined`, never trusted as a real pid (see
/// `authenticate_steps_1_to_3`).
#[derive(Debug, Clone, Copy)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

fn peer_credentials(fd: RawFd) -> io::Result<PeerCredentials> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(cred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCredentials {
        pid: cred.pid as u32,
        uid: cred.uid,
        gid: cred.gid,
    })
}

/// `/proc/<pid>/stat` field 22 ("starttime", ticks since boot) -- the
/// SAME clock/unit [`boot_ticks_now`] reports. Parsed ROBUSTLY: `comm`
/// (field 2) is whatever `/proc/<pid>/comm` held at the last exec/rename,
/// wrapped in parentheses, and MAY ITSELF CONTAIN SPACES AND PARENTHESES
/// -- so `parse_start_ticks` locates the LAST `)` in the line first
/// (the kernel guarantees no field after `comm` ever contains one), then
/// counts whitespace-separated tokens from there.
pub fn process_start_ticks(pid: u32) -> io::Result<u64> {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    parse_start_ticks(&contents).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unparseable /proc/{pid}/stat"),
        )
    })
}

/// The pure parser [`process_start_ticks`] delegates to -- exposed to a
/// unit test below that feeds a literal `/proc/pid/stat`-shaped line with
/// a `comm` containing spaces and parentheses, with no real `/proc`
/// needed.
fn parse_start_ticks(line: &str) -> Option<u64> {
    // Skip past `pid` (field 1) and `comm` (field 2, parenthesized,
    // possibly containing spaces/parens of its own) by finding the LAST
    // `)` in the line -- everything after it is fields 3.. separated by
    // single spaces, per `proc(5)`. Field 22 overall is therefore the
    // 20th whitespace-separated token in that remainder (22 - 2 already
    // consumed).
    let close = line.rfind(')')?;
    let rest = line.get(close + 1..)?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// This process's own start time, in the same units -- the value a Unix
/// server reports as its `created` on the wire (LU2/LU3's `status_ok`);
/// the wire compares `created` for equality only.
pub fn self_start_ticks() -> io::Result<u64> {
    process_start_ticks(std::process::id())
}

/// `CLOCK_BOOTTIME`, converted to the SAME clock ticks
/// [`process_start_ticks`] reports (`sysconf(_SC_CLK_TCK)`, matching
/// `/proc/<pid>/stat` field 22's own unit) -- integer arithmetic
/// throughout, no floating point.
pub fn boot_ticks_now() -> io::Result<u64> {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clk_tck <= 0 {
        return Err(io::Error::other(
            "sysconf(_SC_CLK_TCK) returned a non-positive value",
        ));
    }
    let clk_tck = clk_tck as u64;
    let secs_ticks = (ts.tv_sec as u64).saturating_mul(clk_tck);
    let nanos_ticks = (ts.tv_nsec as u64) * clk_tck / 1_000_000_000;
    Ok(secs_ticks.saturating_add(nanos_ticks))
}

// ---------------------------------------------------------------------
// UAPI constants this crate defines locally rather than depend on a
// specific `libc` version already exposing them (ADR 0043 decision 8:
// "libc may lack it").
// ---------------------------------------------------------------------

/// `SO_PEERPIDFD` (`include/uapi/asm-generic/socket.h`): the same value
/// on every architecture this crate ships to except sparc/mips/powerpc
/// (not currently supported targets).
const SO_PEERPIDFD: libc::c_int = 77;

/// `PIDFD_INFO_EXIT` (`include/uapi/linux/pidfd.h`).
const PIDFD_INFO_EXIT: u64 = 1 << 3;

/// `struct pidfd_info` (`include/uapi/linux/pidfd.h`), the exact v0
/// layout `PIDFD_GET_INFO` fills in. Only `mask` (in/out) and `exit_code`
/// (out) are meaningfully read by this module; the rest exist so the
/// struct's SIZE (and therefore the ioctl's own encoded size field)
/// matches what the kernel expects to write into.
#[repr(C)]
#[derive(Default)]
#[allow(dead_code)] // byte-layout-only fields; see the struct's own doc
struct PidfdInfo {
    mask: u64,
    cgroupid: u64,
    pid: u32,
    tgid: u32,
    ppid: u32,
    ruid: u32,
    rgid: u32,
    euid: u32,
    egid: u32,
    suid: u32,
    sgid: u32,
    fsuid: u32,
    fsgid: u32,
    exit_code: i32,
}

const PIDFS_IOCTL_MAGIC: u32 = 0xFF;

/// `PIDFD_GET_INFO` (`include/uapi/linux/pidfd.h`): `_IOWR(0xFF, 11,
/// struct pidfd_info)`. Encoded by hand via the generic
/// `include/uapi/asm-generic/ioctl.h` `_IOC` formula (dir=`_IOC_READ|
/// _IOC_WRITE`=3, nrbits=8, typebits=8, sizebits=14, dirbits=2 -- the
/// "else" branch every architecture but sparc/mips/powerpc/alpha uses):
/// `(dir << 30) | (size << 16) | (type << 8) | nr`. Verified on this host
/// (kernel 5.15, which lacks the ioctl itself) to reach the pidfd ioctl
/// dispatcher and fail `ENOTTY` -- not `EINVAL`/`ENOSYS` -- confirming the
/// encoding is at least well-formed for a pidfd, before this module's own
/// `ENOTTY` fallback was written against it.
const PIDFD_GET_INFO: libc::Ioctl = {
    let size = std::mem::size_of::<PidfdInfo>() as u32;
    (((3u32) << 30) | (size << 16) | (PIDFS_IOCTL_MAGIC << 8) | 11) as libc::Ioctl
};

/// `pidfd_open(2)`: no safe wrapper function exists in `libc` (only the
/// syscall NUMBER is exported), so this goes through the raw `syscall(2)`
/// the ADR names.
fn pidfd_open(pid: u32) -> io::Result<OwnedFd> {
    let rc = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0 as libc::c_uint) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a non-negative return from pidfd_open(2) is a freshly
    // opened, valid, uniquely-owned fd.
    Ok(unsafe { OwnedFd::from_raw_fd(rc as RawFd) })
}

/// The pidfd's own reported pid, read via `/proc/self/fdinfo/<fd>`'s
/// `Pid:` line -- the cross-check `pin_peer` runs against `SO_PEERPIDFD`'s
/// result before trusting it (a mismatch between the two mechanisms is
/// itself `Undetermined`, never silently trusted).
fn fdinfo_pid(pidfd: RawFd) -> io::Result<u32> {
    let contents = std::fs::read_to_string(format!("/proc/self/fdinfo/{pidfd}"))?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("Pid:") {
            return rest.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "unparseable Pid: line in fdinfo")
            });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "no Pid: line in pidfd's own fdinfo",
    ))
}

/// `SO_PEERPIDFD` (kernel 6.5+): the peer's pidfd straight from the
/// socket, race-free by construction -- no separate `pidfd_open(pid)`
/// call exists for pid reuse to race. `None` = the getsockopt itself
/// reports the option doesn't exist on this kernel: FALL THROUGH to
/// `pidfd_open` in [`pin_peer`], never `Undetermined` for that alone.
fn try_peerpidfd(fd: RawFd, creds: &PeerCredentials) -> Option<ChallengeOutcome<OwnedFd>> {
    let mut raw: libc::c_int = -1;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            SO_PEERPIDFD,
            std::ptr::addr_of_mut!(raw).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        let err = io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::ENOPROTOOPT) | Some(libc::EINVAL) | Some(libc::ENOSYS) => None,
            _ => Some(ChallengeOutcome::Undetermined),
        };
    }
    // SAFETY: `getsockopt` just returned a freshly-duplicated, valid,
    // uniquely-owned fd for this call.
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw) };
    match fdinfo_pid(pidfd.as_raw_fd()) {
        Ok(pid) if pid == creds.pid => Some(ChallengeOutcome::Proven(pidfd)),
        Ok(_) => Some(ChallengeOutcome::Undetermined), // the two mechanisms disagree -- untrustworthy, not "wrong"
        Err(_) => Some(ChallengeOutcome::Undetermined),
    }
}

/// Common to both pin paths: prove `creds.pid`'s process started
/// STRICTLY before `connect_anchor` (a tie is `Undetermined` -- a
/// recycled pid can only belong to a process created AFTER the original
/// peer died, i.e. after this connection's own pre-connect anchor was
/// sampled), then re-read its start time once more to prove the identity
/// hasn't shifted underneath us between the two reads (defense in depth
/// narrowing the window further; this crate's threat model already
/// excludes a same-uid attacker, ADR 0043 module doc "Security").
fn validate_pin(
    pidfd: OwnedFd,
    creds: &PeerCredentials,
    connect_anchor: u64,
) -> ChallengeOutcome<(OwnedFd, u64)> {
    let start = match process_start_ticks(creds.pid) {
        Ok(s) => s,
        Err(_) => return ChallengeOutcome::Undetermined,
    };
    if start >= connect_anchor {
        return ChallengeOutcome::Undetermined;
    }
    let recheck = match process_start_ticks(creds.pid) {
        Ok(s) => s,
        Err(_) => return ChallengeOutcome::Undetermined,
    };
    if recheck != start {
        return ChallengeOutcome::Undetermined;
    }
    ChallengeOutcome::Proven((pidfd, start))
}

/// Race-free pinning against pid reuse (ADR 0043 decision 8): (1) if
/// `prefer_peerpidfd`, try `SO_PEERPIDFD`, falling through to (2) on an
/// older kernel; (2) `pidfd_open(creds.pid)`; either way, (3) VALIDATE via
/// [`validate_pin`] before ever trusting the result. `#[cfg(any(test,
/// feature = "test-support"))] pub fn pin_peer_for_test` below is the
/// SAME function under a test-reachable name, so the test suite can force
/// either path on any kernel regardless of what it actually supports.
fn pin_peer(
    fd: RawFd,
    creds: &PeerCredentials,
    connect_anchor: u64,
    prefer_peerpidfd: bool,
) -> ChallengeOutcome<(OwnedFd, u64)> {
    if prefer_peerpidfd {
        match try_peerpidfd(fd, creds) {
            Some(ChallengeOutcome::Proven(pidfd)) => {
                return validate_pin(pidfd, creds, connect_anchor)
            }
            Some(ChallengeOutcome::Foreign) => return ChallengeOutcome::Foreign,
            Some(ChallengeOutcome::Undetermined) => return ChallengeOutcome::Undetermined,
            None => {} // fall through: this kernel has no SO_PEERPIDFD
        }
    }
    match pidfd_open(creds.pid) {
        // ESRCH (already gone) and ENOSYS (kernel < 5.3) both fail
        // closed here, like every other OS-call failure in this
        // function -- no numeric-pid signalling fallback exists.
        Ok(pidfd) => validate_pin(pidfd, creds, connect_anchor),
        Err(_) => ChallengeOutcome::Undetermined,
    }
}

/// Steps 1-3 of the OS-side identity check, shared by [`challenge()`] and
/// [`authenticate_server()`]: `SO_PEERCRED` for `(pid, uid, gid)`, same-
/// user comparison BEFORE anything else, then the race-free pin.
fn authenticate_steps_1_to_3(
    conn: &dyn SocketChallengeable,
) -> ChallengeOutcome<(OwnedFd, u32, u64)> {
    // Step 1-2: one getsockopt for pid+uid+gid together.
    let creds = match peer_credentials(conn.raw_fd()) {
        Ok(c) => c,
        Err(_) => return ChallengeOutcome::Undetermined,
    };
    if creds.pid == 0 {
        // A different pid namespace, or credentials otherwise
        // unavailable -- never a real pid to pin against.
        return ChallengeOutcome::Undetermined;
    }

    // Step 3: nothing past this point trusts, decodes, or acts on
    // anything from the peer until same-user equality has been checked
    // (property 20).
    if creds.uid != unsafe { libc::geteuid() } {
        return ChallengeOutcome::Foreign;
    }

    match pin_peer(conn.raw_fd(), &creds, conn.connect_anchor_boot_ticks(), true) {
        ChallengeOutcome::Proven((pidfd, created)) => {
            ChallengeOutcome::Proven((pidfd, creds.pid, created))
        }
        ChallengeOutcome::Foreign => ChallengeOutcome::Foreign,
        ChallengeOutcome::Undetermined => ChallengeOutcome::Undetermined,
    }
}

/// A retained pidfd to a process this crate has PROVEN is the server
/// behind one challenged connection: `(pidfd, pid, start-time ticks)`.
/// Dropping this closes the pidfd. ONLY the full five-step [`challenge()`]
/// ever produces one -- see [`SidAuthenticated`] for the deliberately
/// weaker, deliberately handle-less steps-1-3-only counterpart. Mirrors
/// `challenge_win::ChallengedProcess` in shape; every method below swaps
/// its Win32 mechanism for the Linux one the ADR names.
pub struct ChallengedProcess {
    pidfd: OwnedFd,
    pid: u32,
    created: u64,
}

impl std::fmt::Debug for ChallengedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChallengedProcess")
            .field("pid", &self.pid)
            .field("created", &self.created)
            .finish_non_exhaustive()
    }
}

impl ChallengedProcess {
    /// TEST-SUPPORT ONLY: wrap an already-pinned pidfd (from
    /// [`pin_peer_for_test`]) into a real `ChallengedProcess`, so a test
    /// against a peer that cannot itself speak the wire protocol (a
    /// spawned `sleep` child, say) can still exercise `wait`/`terminate`/
    /// `exit_status_after_confirmed_exit` through their REAL public
    /// methods instead of duplicating this module's own ioctl/syscall
    /// encodings a second time.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_pinned_for_test(pidfd: OwnedFd, pid: u32, created: u64) -> Self {
        Self { pidfd, pid, created }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The start-time ticks this pidfd was PROVEN against (the same
    /// `/proc/<pid>/stat` field-22 unit `boot_ticks_now`/
    /// `process_start_ticks` use) -- the wire's own `status_ok.created`
    /// carries the identical value, compared for equality only.
    pub fn created(&self) -> u64 {
        self.created
    }

    /// Re-read `self.pid`'s start time and compare it against what this
    /// pidfd was proven with -- the ADR's "pre-terminate
    /// re-verification". Unlike the Windows handle (which pins the
    /// kernel process object directly, making `Ok(true)` the only outcome
    /// a correct caller ever sees), a Linux pidfd's underlying process
    /// CAN exit out from under it at any time -- `Ok(false)` on
    /// `ENOENT`/`ESRCH` (the process is simply gone) is an ordinary,
    /// expected outcome here, not a bug.
    pub fn reverify(&self) -> io::Result<bool> {
        match process_start_ticks(self.pid) {
            Ok(start) => Ok(start == self.created),
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT) | Some(libc::ESRCH)) => {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// The death signal a supervisor waits on rather than sampling
    /// process absence: `poll(2)` on the pidfd, which becomes readable
    /// exactly when the process has exited. Bounded, never infinite.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        let mut pfd = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(rc > 0)
    }

    /// The KILL half of the probe's own KILL+WAIT row:
    /// `pidfd_send_signal(SIGKILL)`, race-free against pid reuse because
    /// it targets the pidfd, never a numeric pid.
    pub fn terminate(&self) -> io::Result<()> {
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::c_void>(),
                0u32,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `PIDFD_GET_INFO` with `PIDFD_INFO_EXIT` (kernel 6.15+), mirroring
    /// `ChallengedProcess::exit_code_after_confirmed_exit`'s own
    /// precondition: the caller must have already observed
    /// [`wait`](Self::wait) return `true`. `Ok(None)` -- "exited, status
    /// unknown" -- covers BOTH a kernel too old for the ioctl at all
    /// (`ENOTTY`/`EINVAL`/`ENOSYS`) and a kernel that answers but did not
    /// actually set `PIDFD_INFO_EXIT` in the returned mask; `Ok(Some(_))`
    /// is only ever returned once the kernel has affirmatively reported
    /// the exit code.
    pub fn exit_status_after_confirmed_exit(&self) -> io::Result<Option<i32>> {
        let mut info = PidfdInfo {
            mask: PIDFD_INFO_EXIT,
            ..Default::default()
        };
        let rc = unsafe {
            libc::ioctl(
                self.pidfd.as_raw_fd(),
                PIDFD_GET_INFO,
                std::ptr::addr_of_mut!(info),
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            return match err.raw_os_error() {
                Some(libc::ENOTTY) | Some(libc::EINVAL) | Some(libc::ENOSYS) => Ok(None),
                _ => Err(err),
            };
        }
        if info.mask & PIDFD_INFO_EXIT == 0 {
            return Ok(None);
        }
        Ok(Some(info.exit_code))
    }
}

/// The five pinned steps (ADR 0041 Lifecycle "The challenge", ADR 0043
/// decision 8), in order: (1-2) `SO_PEERCRED` for `(pid, uid, gid)`; (3)
/// same-user comparison; (4) only then `exchange`'s request on the SAME
/// connection; (5) proven iff same-user matched AND reply-pid == the
/// observed pid AND reply creation time == the pinned pidfd's own start
/// time -- pid compared FIRST, creation time checked only if it matches
/// (mirrors `challenge_win::challenge`'s own ordering: a provably wrong
/// pid must never become `Undetermined` merely because something else
/// also failed). Nothing in the reply is decoded for meaning or acted on
/// before step 3 succeeds.
///
/// `reply_deadline` bounds ONLY steps 4-5 -- steps 1-3 are local,
/// synchronous OS calls with no wait to bound. `Proven` here means the
/// FULL five-step proof -- see [`authenticate_server()`] for the
/// deliberately separate, deliberately weaker steps-1-3-only operation.
pub fn challenge(
    conn: &dyn SocketChallengeable,
    exchange: &mut dyn IdentityExchange,
    reply_deadline: Instant,
) -> ChallengeOutcome<ChallengedProcess> {
    let (pidfd, pid, created) = match authenticate_steps_1_to_3(conn) {
        ChallengeOutcome::Proven(v) => v,
        ChallengeOutcome::Foreign => return ChallengeOutcome::Foreign,
        ChallengeOutcome::Undetermined => return ChallengeOutcome::Undetermined,
    };

    // Steps 4-5: the lane's own request/reply, the shared, platform-
    // neutral wire half (L1-unix LU1a) -- see
    // `crate::challenge::exchange_identity`'s own doc.
    let c: &dyn ChallengeableConnection = conn;
    let exchange_result = exchange_identity(c, exchange, reply_deadline);

    let (reply_pid, reply_created) = match exchange_result {
        None => return ChallengeOutcome::Undetermined,
        Some(Ok(v)) => v,
        Some(Err(StatusFailure::Foreign)) => return ChallengeOutcome::Foreign,
        Some(Err(StatusFailure::Undetermined)) => return ChallengeOutcome::Undetermined,
    };

    // Property 22: pid compared FIRST, so a provably wrong pid is ALWAYS
    // Foreign, never Undetermined.
    if reply_pid != pid {
        return ChallengeOutcome::Foreign;
    }
    if reply_created != created {
        return ChallengeOutcome::Foreign;
    }

    ChallengeOutcome::Proven(ChallengedProcess {
        pidfd,
        pid,
        created,
    })
}

/// ADR 0041 Lifecycle "The challenge", steps 1-3 ONLY: identify the peer
/// process behind a live connection and authenticate its same-user
/// identity. No wire I/O of any kind -- see `challenge_win::
/// authenticate_server`'s own doc for why the shared, lane-agnostic
/// connect constructor can only ever offer this, never the full proof.
/// The pidfd IS dropped here (no retained object; `SidAuthenticated` is
/// the deliberately weaker type -- property 25).
pub fn authenticate_server(conn: &dyn SocketChallengeable) -> SidAuthOutcome {
    match authenticate_steps_1_to_3(conn) {
        ChallengeOutcome::Foreign => SidAuthOutcome::Foreign,
        ChallengeOutcome::Undetermined => SidAuthOutcome::Undetermined,
        ChallengeOutcome::Proven((_pidfd, pid, created)) => {
            SidAuthOutcome::Authenticated(SidAuthenticated { pid, created })
        }
    }
}

/// TEST-SUPPORT ONLY: exercises [`pin_peer`] with an explicit
/// `prefer_peerpidfd` choice, so the test suite can drive BOTH pin paths
/// deterministically on any kernel (GitHub runners may have
/// `SO_PEERPIDFD`; the backend hosts, kernel 5.15, do not).
#[cfg(any(test, feature = "test-support"))]
pub fn pin_peer_for_test(
    fd: RawFd,
    creds: &PeerCredentials,
    connect_anchor: u64,
    prefer_peerpidfd: bool,
) -> ChallengeOutcome<(OwnedFd, u64)> {
    pin_peer(fd, creds, connect_anchor, prefer_peerpidfd)
}

#[cfg(test)]
mod tests {
    use super::parse_start_ticks;

    /// A literal `/proc/pid/stat`-shaped line whose `comm` field (the
    /// second, parenthesized field) contains BOTH a space and a nested
    /// close-paren -- the exact shape that breaks a naive
    /// first-`(`-to-first-`)` parse. Field 22 ("starttime") is the 20th
    /// token after the LAST `)`.
    #[test]
    fn parses_starttime_past_a_comm_with_spaces_and_parens() {
        // pid=1234 comm="weird (proc) name" state=S ppid..majflt (fields
        // 4-9, six tokens) utime cutime cstime priority nice num_threads
        // itrealvalue (fields 14-20, seven tokens) starttime=987654321
        // (field 22) ...
        let line = "1234 (weird (proc) name) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 0 20 0 1 987654321 0 0";
        assert_eq!(parse_start_ticks(line), Some(987654321));
    }

    #[test]
    fn rejects_a_line_with_no_closing_paren() {
        assert_eq!(parse_start_ticks("no parens here at all"), None);
    }

    #[test]
    fn rejects_a_line_with_too_few_fields_after_comm() {
        assert_eq!(parse_start_ticks("1234 (comm) S 1 1"), None);
    }
}
