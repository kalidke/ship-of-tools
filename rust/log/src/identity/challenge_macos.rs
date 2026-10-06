//! macOS half of the same-connection challenge (ADR 0043 decision 8):
//! steps 1-3 are two reads of the connected socket, the peer's account
//! from the credentials the kernel cached for the connection (`getpeereid`)
//! and its pid and pidversion from `LOCAL_PEERTOKEN`, plus the identity a proof returns
//! ([`ChallengedProcess`]). Steps 4-5 (the wire half) are shared,
//! platform-neutral logic in `crate::identity::challenge` -- see that module's own
//! doc; [`challenge()`] and [`authenticate_server()`] below call into it
//! rather than reimplementing it. Mirrors `challenge_unix.rs` in SHAPE,
//! and is deliberately a fraction of its length -- why, is the whole
//! point of this module (see "What macOS does not need" below).
//!
//! # The kernel facts this rests on
//!
//! On a connected `AF_UNIX` socket, either end can read two things about
//! its peer:
//!
//! * `getpeereid`: the credentials the kernel cached for the connection,
//!   the listener's at `listen()` as a client sees them and the client's
//!   at `connect(2)` as a server sees them. Nothing the peer does later
//!   changes them, so the account (step 3) is read here
//!   (`tests/challenge_macos.rs`'s credential-transition tests pin it).
//! * `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)`: the 32-byte `audit_token_t`
//!   of the process at the peer end's `last_pid`, looked up when it is
//!   called, carrying its pid (word 5) and its `pidversion` (word 7), the
//!   kernel's monotonic process-creation generation. It names a process,
//!   never the account: its euid word is not read.
//!
//! THE DIRECTION IS WHAT MAKES [`authenticate_server()`] POSSIBLE AT ALL,
//! exactly as on the other two platforms: the CLIENT's own fd is where the
//! peer observations are read. `tests/macos_kernel_facts/` pins that a
//! client's token names the live test server; a kernel that changed this would
//! report itself as that named red test, not as a silent auth regression.
//!
//! # What macOS does NOT need
//!
//! Most of `challenge_unix.rs` exists to RECONSTRUCT, out of two
//! independent mechanisms, precisely what one Darwin token already
//! carries: a pid together with the generation that distinguishes THAT
//! instance from a later process recycling the same number. Linux needs
//! `/proc/<pid>/stat` field 22, `CLOCK_BOOTTIME` converted into the same
//! ticks, a pre-connect anchor the transport has to sample and carry,
//! `SO_PEERPIDFD` (or `pidfd_open` plus an `fdinfo` cross-check to catch
//! the two mechanisms disagreeing), and a double read to narrow the
//! window between them. Darwin's token carries `(pid, pidversion)`
//! TOGETHER, from one kernel read: there is no second mechanism to
//! reconcile and no anchor to sample. It is not latched at `connect(2)`,
//! though: the kernel looks the peer's pid up when the token is read, so
//! once the peer has exited and its pid is reused the token describes the
//! new process. Attributing that process to the responder requires an
//! honest reply reporting its own identity (step 5); the death watch's
//! liveness argument also requires a reply after reading the request.
//! The account never rests on them. Steps 1-3 are two reads and three
//! comparisons, and [`SocketChallengeable`] here has no
//! `connect_anchor_boot_ticks` twin -- `socket_unix::SocketClient`'s own
//! anchor field stays unread (and `#[allow(dead_code)]`) on this target.
//!
//! # `created` is the pidversion
//!
//! Every platform's `created` is "whatever unit this OS's own
//! `status_ok.created` carries, compared for equality only"
//! (`client::PeerIdentity::created`): Windows packs the creation
//! `FILETIME`, Linux the `/proc` start ticks, macOS the `pidversion`.
//! It is the right unit here because it distinguishes process instances.
//! A matching reply attributes that instance to its sender only when the
//! responder honestly reports its own identity.
//! [`self_pidversion()`] is the self-facing twin a server reports with,
//! the macOS counterpart of `challenge_unix::self_start_ticks`.
//!
//! # The death watch (milestone M3b)
//!
//! macOS has no pidfd, and nothing else here keeps a process object
//! alive by reference. Its one equivalent is a `kqueue`
//! `EVFILT_PROC`/`NOTE_EXIT` registration: the knote attaches to a
//! specific `proc`, so -- exactly like a pidfd, and unlike a pid number
//! -- it names the process INSTANCE. [`ChallengedProcess`] retains one,
//! one `kqueue` fd per handle, closed by drop; that handle is what lets
//! this type implement [`crate::lane::client::PeerProcess`] with the SAME
//! contract as its two siblings rather than a weaker macOS spelling of
//! it.
//!
//! Two kernel differences the Linux code does not have to absorb, both
//! load-bearing:
//!
//! * `NOTE_EXIT` is delivered ONCE and the kernel then detaches the
//!   knote, where a pidfd stays readable forever. [`ChallengedProcess`]
//!   therefore carries an `AtomicBool` latch, which is not a cache: it
//!   is what supplies the stickiness the pidfd gets from the kernel for
//!   free. Every entry point reads it first and writes it on any
//!   observed exit.
//! * `kevent`'s timeout is a POINTER, and NULL means "block forever".
//!   `Duration::ZERO` must become `timespec { 0, 0 }` -- see
//!   [`kevent_timeout`], the one conversion in this module whose mistake
//!   would type-check, pass review, and hang the supervisor's own tick.
//!
//! `reverify` is the same contract as Linux's by a different mechanism
//! (see [`ChallengedProcess::reverify`]); `terminate` is the one place
//! macOS is genuinely weaker, and [`ChallengedProcess::terminate`] says
//! exactly what makes it safe anyway.

#![cfg(target_os = "macos")]

use crate::identity::challenge::{
    exchange_identity, ChallengeOutcome, ChallengeableConnection, PeerAuthOutcome, PeerAuthenticated,
    StatusFailure,
};
use crate::lane::client::{PeerIdentity, PeerProcess};
use crate::identity::exchange::IdentityExchange;
use super::exit_watch_macos::{drain_exit, watch_exit};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The macOS-shaped extension every [`ChallengeableConnection`] this
/// crate actually challenges must also supply: raw fd access, for the
/// two reads steps 1-3 consist of -- the macOS twin of
/// `challenge_win::PipeChallengeable` and `challenge_unix::
/// SocketChallengeable`. Deliberately ONE method: the Linux twin's
/// second one (`connect_anchor_boot_ticks`) exists only to anchor a pin
/// this platform gets from the kernel already (module doc, "What macOS
/// does not need").
pub trait SocketChallengeable: ChallengeableConnection {
    /// This end's own fd for the connected socket -- `getpeereid` and
    /// `LOCAL_PEERTOKEN` both answer about the peer from either end (see this
    /// module's own doc), so the CLIENT's own fd is exactly what steps 1-3 need.
    fn raw_fd(&self) -> RawFd;
}

/// `audit_token_t` -- eight `u32`s, in the order Apple's
/// `audit_token_to_*` accessors define: auid, euid, egid, ruid, rgid,
/// pid, asid, pidversion. Declared here rather than taken from `libc`,
/// which exports the `SOL_LOCAL`/`LOCAL_PEERTOKEN` constants for apple
/// targets but not this struct -- the same local declaration
/// `tests/macos_kernel_facts/` already proved the shape of.
#[repr(C)]
#[derive(Clone, Copy)]
struct AuditToken {
    val: [u32; 8],
}

const TOK_PID: usize = 5;
const TOK_PIDVERSION: usize = 7;

/// The two words of the peer's audit token this module reads, from ONE
/// `getsockopt`. `LOCAL_PEERTOKEN` looks the peer's pid up when it is
/// called, so they describe the process holding that pid at that moment:
/// a process, never the account (see [`peer_euid_pid_created`]).
struct PeerToken {
    pid: u32,
    /// The kernel's process-creation generation for `pid`, as of the read.
    /// Zero is treated as "no generation" and never trusted -- see
    /// [`authenticate_steps_1_to_3`].
    pidversion: u32,
}

fn peer_token(fd: RawFd) -> io::Result<PeerToken> {
    let mut token = AuditToken { val: [0u32; 8] };
    let mut len = std::mem::size_of::<AuditToken>() as libc::socklen_t;
    // SAFETY: `fd` is a live socket owned by the caller for the whole
    // call; the out-buffer is a local `audit_token_t`-shaped struct and
    // `len` is its true size, which the kernel may only shrink.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            std::ptr::addr_of_mut!(token).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // A SHORT token is not a partial answer to be salvaged: the words
    // this module reads live at fixed offsets, and a kernel that filled
    // fewer of them would hand back zeros (or stale stack) at exactly
    // the positions identity is read from. Fail closed instead.
    if len as usize != std::mem::size_of::<AuditToken>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("LOCAL_PEERTOKEN returned {len} bytes, not a whole audit_token_t"),
        ));
    }
    Ok(PeerToken {
        pid: token.val[TOK_PID],
        pidversion: token.val[TOK_PIDVERSION],
    })
}

/// The peer's account: the effective uid in the credentials the kernel
/// cached for the connection (`getpeereid`), the listener's at `listen()`
/// as a client sees it and the client's at `connect(2)` as a server sees
/// it. Nothing the peer does later changes it.
fn peer_euid(fd: RawFd) -> io::Result<u32> {
    let (mut euid, mut egid) = (0, 0);
    // SAFETY: `fd` is a live socket owned by the caller for the whole
    // call; both out-pointers are locals.
    if unsafe { libc::getpeereid(fd, &mut euid, &mut egid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(euid)
}

/// The one macOS reader of a connected peer: its euid from [`peer_euid`],
/// which decides the account, and its pid and pidversion (the macOS
/// `created`) from [`peer_token`], which name a process. The daemon admits
/// a connection by the euid and keeps the other two for a lease; steps 1-3
/// read the same three.
pub fn peer_euid_pid_created(fd: RawFd) -> io::Result<(u32, u32, u64)> {
    let euid = peer_euid(fd)?;
    let t = peer_token(fd)?;
    Ok((euid, t.pid, u64::from(t.pidversion)))
}

/// `TASK_AUDIT_TOKEN` (`osfmk/mach/task_info.h`): the `task_info`
/// flavor that yields the calling task's own `audit_token_t`. `libc`
/// exports `task_info` and `mach_task_self` for apple targets but not
/// this flavor constant, so it is declared here -- and
/// [`self_pidversion`]'s own test cross-checks the value it produces
/// against the SAME process's token as observed through a socket, so a
/// wrong constant fails as one named red test rather than as a silently
/// unprovable server.
const TASK_AUDIT_TOKEN: libc::task_flavor_t = 15;

/// This process's own `pidversion` -- the value a macOS server reports
/// as its `created` on the wire, the counterpart of
/// `challenge_unix::self_start_ticks`. There is no socket to read it
/// from (a server's own fd carries its CLIENT's token, not its own), so
/// it comes from the task port instead; `task_info(TASK_AUDIT_TOKEN)`
/// on `mach_task_self()` needs no entitlement and no privilege, unlike
/// `task_for_pid` on anyone else.
///
/// `#[allow(deprecated)]`: `libc::mach_task_self` is deprecated in
/// favour of the `mach2` crate, which this workspace does not depend on
/// and which would be a whole new dependency for one port constant.
#[allow(deprecated)]
pub fn self_pidversion() -> io::Result<u32> {
    let mut token = AuditToken { val: [0u32; 8] };
    // `task_info`'s count is in `integer_t` units, not bytes: 32/4 = 8.
    let mut count = (std::mem::size_of::<AuditToken>() / std::mem::size_of::<libc::integer_t>())
        as libc::mach_msg_type_number_t;
    // SAFETY: `mach_task_self()` is this task's own send right (never
    // consumed here); the out-buffer is a local `audit_token_t`-shaped
    // struct and `count` its true length in `integer_t` units, which the
    // kernel may only shrink.
    let kr = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            TASK_AUDIT_TOKEN,
            std::ptr::addr_of_mut!(token).cast(),
            &mut count,
        )
    };
    if kr != 0 {
        // KERN_SUCCESS is 0; every other `kern_return_t` is a failure
        // with no `errno` behind it, so the code itself is the detail.
        return Err(io::Error::other(format!(
            "task_info(TASK_AUDIT_TOKEN) failed: kern_return_t {kr}"
        )));
    }
    if (count as usize) < TOK_PIDVERSION + 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("task_info(TASK_AUDIT_TOKEN) filled only {count} words of audit_token_t"),
        ));
    }
    Ok(token.val[TOK_PIDVERSION])
}

/// Steps 1-3 of the OS-side identity check, shared by [`challenge()`]
/// and [`authenticate_server()`]: the one reader
/// ([`peer_euid_pid_created`]), the two "is this even an identity"
/// rejections, then the same-user comparison on the cached credential's
/// euid. Returns `(pid, created)`; `Foreign`/`Undetermined` are already
/// the caller's own terminal outcome.
fn authenticate_steps_1_to_3(conn: &dyn SocketChallengeable) -> ChallengeOutcome<(u32, u64)> {
    // Steps 1-2: the account from the cached credential, the pid and
    // pidversion from the token.
    let (euid, pid, created) = match peer_euid_pid_created(conn.raw_fd()) {
        Ok(v) => v,
        Err(_) => return ChallengeOutcome::Undetermined,
    };
    if pid == 0 {
        // No peer pid at all -- never a real pid to bind a reply to
        // (the twin of `challenge_unix`'s own `creds.pid == 0` arm).
        return ChallengeOutcome::Undetermined;
    }
    if created == 0 {
        // A pid with no generation behind it is exactly the identity
        // Linux refuses to proceed on without a pidfd: the number alone
        // cannot distinguish this process from its successor. Not
        // `Foreign` -- nothing here says the peer is WRONG, only that it
        // is unprovable (`tests/macos_kernel_facts/` pins a real peer
        // as having a non-zero one).
        return ChallengeOutcome::Undetermined;
    }

    // Step 3: nothing past this point trusts, decodes, or acts on
    // anything from the peer until same-user equality has been checked
    // (property 20).
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    if euid != unsafe { libc::geteuid() } {
        return ChallengeOutcome::Foreign;
    }

    ChallengeOutcome::Proven((pid, created))
}

/// A challenged connection's process handle, together with the retained
/// `kqueue` death watch: `(kqueue, pid, pidversion)`. The last two are
/// live token observations matched by step 5; attributing the watched
/// instance to the responder requires it to report its own identity
/// honestly after reading the request. Dropping this
/// closes the kqueue and with it the knote. ONLY the full five-step
/// [`challenge()`] ever produces one -- see [`PeerAuthenticated`] for
/// the deliberately weaker, deliberately watch-less steps-1-3-only
/// counterpart.
///
/// Mirrors `challenge_win::ChallengedProcess` and
/// `challenge_unix::ChallengedProcess` in name, shape and contract;
/// every method below swaps the sibling's mechanism for the macOS one.
/// Deliberately NOT `Copy`/`Clone` (it owns an fd), exactly like both
/// siblings.
pub struct ChallengedProcess {
    /// The kqueue holding this handle's one `NOTE_EXIT` knote -- see
    /// [`watch_exit`]. Registered BEFORE the reply that proves the
    /// identity was asked for; [`challenge()`] argues why that ordering
    /// is the whole game.
    kq: OwnedFd,
    pid: u32,
    created: u64,
    /// The stickiness a pidfd gets from the kernel and a knote does not:
    /// `NOTE_EXIT` is delivered ONCE and the knote is then detached, so
    /// without this a second `wait` would wait out its whole timeout and
    /// answer `false` about a process it had already watched die. Every
    /// entry point reads it first and writes it on any observed exit.
    ///
    /// `AtomicBool`, not `Cell<bool>`: the Linux handle is `Send + Sync`
    /// and `supervisor_client` stores it inside shared state, so a `Cell`
    /// would compile every generic consumer on Linux and fail it here.
    /// Pinned by [`_ASSERT_SHARABLE`] below.
    exited: AtomicBool,
}

/// The Linux handle is `Send + Sync` and generic code above this seam
/// relies on it (`supervisor_client`'s shared state). A macOS handle
/// that quietly lost either would compile on Linux and break only here,
/// which is precisely the failure this const block converts into a
/// compile error in the file that could cause it.
const _ASSERT_SHARABLE: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ChallengedProcess>();
};

impl std::fmt::Debug for ChallengedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChallengedProcess")
            .field("pid", &self.pid)
            .field("created", &self.created)
            .finish_non_exhaustive()
    }
}

impl ChallengedProcess {
    /// TEST-SUPPORT ONLY: wrap an already-registered watch (from
    /// [`watch_exit`]) into a real `ChallengedProcess`, so a test
    /// against a peer that cannot itself speak the wire protocol (a
    /// spawned `sleep` child, say) can still exercise
    /// `wait`/`terminate`/`reap` through their REAL public methods
    /// instead of duplicating this module's own `kevent` encodings a
    /// second time. The macOS twin of
    /// `challenge_unix::ChallengedProcess::from_pinned_for_test`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_watch_for_test(kq: OwnedFd, pid: u32, created: u64) -> Self {
        Self {
            kq,
            pid,
            created,
            exited: AtomicBool::new(false),
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The `pidversion` this identity was PROVEN against -- the unit the
    /// macOS wire's own `status_ok.created` carries (see the module doc,
    /// "`created` is the pidversion"), compared for equality only.
    pub fn created(&self) -> u64 {
        self.created
    }

    /// The one place the latch and the kqueue are read together, and the
    /// only path any of the four public methods below takes to the
    /// kernel: answer from the latch if the exit was EVER observed,
    /// otherwise drain this handle's own kqueue for `timeout` and latch
    /// what comes back.
    fn observe_exit(&self, timeout: Duration) -> io::Result<bool> {
        if self.exited.load(Ordering::Acquire) {
            return Ok(true);
        }
        let exited = drain_exit(self.kq.as_raw_fd(), timeout)?;
        if exited {
            self.exited.store(true, Ordering::Release);
        }
        Ok(exited)
    }

    /// The ADR's "pre-terminate re-verification", and the SAME contract
    /// as the Linux twin's -- *`Ok(true)` iff the pid still names the
    /// instance this handle was proven against; `Ok(false)`, including
    /// the "it is simply gone" case, is an ordinary expected outcome,
    /// not a bug* -- reached by a different mechanism.
    ///
    /// It asks this handle's kqueue, non-blocking, rather than reading the
    /// peer token again: the knote is bound to the INSTANCE, so an
    /// undelivered exit concerns the process attached at registration,
    /// where a token read would describe whatever process holds the pid
    /// now. Attributing that watched process to the responder requires
    /// an honest self-identity reply after reading the request. The watch
    /// answers about one kernel object rather than comparing two samples.
    pub fn reverify(&self) -> io::Result<bool> {
        Ok(!self.observe_exit(Duration::ZERO)?)
    }

    /// The death signal a supervisor waits on rather than sampling
    /// process absence: `kevent(2)` on this handle's own kqueue, which
    /// reports exactly when the watched INSTANCE has exited. Bounded,
    /// never infinite -- `Duration::ZERO` is the supervisor's own
    /// non-blocking tick and must reach the kernel as
    /// `timespec { 0, 0 }`, never a NULL pointer (see
    /// [`kevent_timeout`]).
    ///
    /// Deliberately does NOT reap (ADR 0043 decision 21): a leg this
    /// `ChallengedProcess` identifies may be ADOPTED (not this
    /// supervisor's own child at all) -- see [`Self::reap`], the
    /// explicit, owner-called reap once a caller has observed the exit.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        self.observe_exit(timeout)
    }

    /// The KILL half of the probe's own KILL+WAIT row, and the
    /// invalid-mgmt fallback's hard stop. macOS has no
    /// `pidfd_send_signal` twin -- `task_for_pid` is entitlement-gated
    /// and there is no other handle-addressed signal -- so this is
    /// `kill(2)` BY NUMBER, which is the pid-reuse hazard this whole
    /// module exists to close. Three things make it safe anyway, and
    /// they are the reason this is not simply a bare `kill`:
    ///
    /// 1. **The un-fired registration is checked first.** This handle's
    ///    knote is bound to the instance; if it has already fired, the
    ///    number is no longer OURS to aim at and no signal is sent. So
    ///    the only pid ever signalled is one this process holds a live,
    ///    instance-bound watch on.
    /// 2. For a leg this supervisor SPAWNED and has not reaped, that is
    ///    exactly as safe as the Linux call: `SIGCHLD` is `SIG_DFL` for
    ///    the supervisor's whole life and nothing auto-reaps, so an
    ///    exited child is a retained zombie and its pid CANNOT be
    ///    recycled.
    /// 3. For an ADOPTED leg the residual is bounded by the pid
    ///    allocator, not by hand-waving: Darwin allocates sequentially
    ///    and wraps at `PID_MAX`, so recycling one specific number takes
    ///    on the order of 10^5 intervening process creations, against a
    ///    window of two adjacent syscalls -- and that bound does not
    ///    shrink under load (a fork storm makes the 10^5 creations take
    ///    LONGER, not the window longer).
    ///
    /// The proof of death is never this call. `Ok(())` here means "the
    /// signal was sent", and nothing may report a leg terminated on its
    /// strength alone -- the authoritative half is [`Self::wait`], which
    /// IS instance-bound, so the worst case of a mis-aimed kill is a
    /// stray `SIGKILL` at a same-euid process, never a false "the leg is
    /// gone".
    pub fn terminate(&self) -> io::Result<()> {
        // A poll that ERRORS says nothing about liveness, so it must not
        // suppress the kill: only a confirmed exit does.
        if matches!(self.observe_exit(Duration::ZERO), Ok(true)) {
            return Ok(());
        }
        // SAFETY: `kill` takes a pid and a signal number and cannot
        // touch this process's memory.
        let rc = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The single, explicit reap point, the macOS half of
    /// `challenge_unix::ChallengedProcess::reap`'s own contract:
    /// `waitpid(pid, WNOHANG)`, result ignored.
    ///
    /// Linux reaps through the PIDFD precisely so the numeric pid can
    /// never be misread, and macOS has no such addressing. The latch is
    /// what stands in for it: this returns without calling `waitpid` at
    /// all unless THIS handle's own instance-bound knote has already
    /// reported the exit, so the number named here is one that a live
    /// zombie is still holding down (`SIGCHLD` is `SIG_DFL` and nothing
    /// auto-reaps, so an exited child of ours cannot have had its pid
    /// recycled). A caller that has not observed the exit gets no
    /// syscall rather than a guess.
    ///
    /// The single-owner rule is unchanged: no `Drop` impl exists on this
    /// type (dropping just closes the kqueue, via `OwnedFd`'s own
    /// `Drop`) -- the OWNER reaps, explicitly, only after `wait` returned
    /// `true` and it has read everything it wanted. `self.pid` not being
    /// ours to reap (an adopted leg belonging to a DIFFERENT, earlier
    /// supervisor, or a peer this handle merely challenged) reports
    /// `ECHILD` -- the harmless, expected answer for a non-owner,
    /// exactly as on Linux.
    pub fn reap(&self) {
        if !self.exited.load(Ordering::Acquire) {
            return;
        }
        let mut status: libc::c_int = 0;
        // SAFETY: `waitpid` writes only into `status`, and `WNOHANG`
        // means it never blocks.
        unsafe {
            libc::waitpid(self.pid as libc::pid_t, &mut status, libc::WNOHANG);
        }
    }
}

/// L1-unix LU3a (ADR 0043 decision 19): the seam trait every concrete
/// `ChallengedProcess` implements -- `pid`/`created`/`reverify`/`wait`/
/// `terminate` already have these exact signatures, so this is pure
/// delegation.
impl PeerIdentity for ChallengedProcess {
    fn pid(&self) -> u32 {
        ChallengedProcess::pid(self)
    }

    fn created(&self) -> u64 {
        ChallengedProcess::created(self)
    }
}

impl PeerProcess for ChallengedProcess {
    fn reverify(&self) -> io::Result<bool> {
        ChallengedProcess::reverify(self)
    }

    fn wait(&self, timeout: Duration) -> io::Result<bool> {
        ChallengedProcess::wait(self, timeout)
    }

    fn terminate(&self) -> io::Result<()> {
        ChallengedProcess::terminate(self)
    }
}

/// The five pinned steps (ADR 0041 Lifecycle "The challenge", ADR 0043
/// decision 8), in order: (1-2) [`peer_euid_pid_created`], the euid
/// from `getpeereid` and `(pid, pidversion)` from `LOCAL_PEERTOKEN`; (3) same-user
/// comparison of that euid; (4) only then
/// `exchange`'s request on the SAME connection; (5) proven iff same-user
/// matched AND reply-pid == the observed pid AND reply creation == the
/// observed pidversion -- pid compared FIRST (mirrors both siblings'
/// ordering: a provably wrong pid must ALWAYS be `Foreign`, never
/// `Undetermined` because something else also failed). Nothing in the
/// reply is decoded for meaning or acted on before step 3 succeeds.
///
/// `reply_deadline` bounds ONLY steps 4-5 -- steps 1-3 are two local,
/// synchronous OS calls with no wait to bound. `Proven` here means all
/// five checks passed; process attribution requires an honest responder
/// reporting its own identity. See [`authenticate_server()`] for the
/// separate steps-1-3-only operation.
///
/// The macOS addition: [`watch_exit`] registration sits BETWEEN step 3
/// and step 4. It binds the watched instance to the responder only when
/// an honest responder reports its own identity after reading the request.
/// It is not a sixth step and changes no outcome except to fail
/// `Undetermined`; the body states this conditional liveness argument.
pub fn challenge(
    conn: &dyn SocketChallengeable,
    exchange: &mut dyn IdentityExchange,
    reply_deadline: Instant,
) -> ChallengeOutcome<ChallengedProcess> {
    let (pid, created) = match authenticate_steps_1_to_3(conn) {
        ChallengeOutcome::Proven(v) => v,
        ChallengeOutcome::Foreign => return ChallengeOutcome::Foreign,
        ChallengeOutcome::Undetermined => return ChallengeOutcome::Undetermined,
    };

    // THE DEATH WATCH IS REGISTERED HERE -- between step 3 and step 4,
    // and the ordering is the entire argument. DO NOT "simplify" it by
    // moving the registration after the exchange:
    //
    //   Registering AFTER the proof can attach to the wrong instance. If
    //   the peer exits and its pid is recycled between the token read and
    //   the `EV_ADD`, the knote attaches to a DIFFERENT process. Its
    //   exit signal then concerns that successor, not the original peer.
    //
    //   Registering HERE closes that race only for an honest responder
    //   reporting its own `(pid, pidversion)` after reading the request:
    //   attach precedes the request, so a matching reply (step 5) then
    //   shows that instance alive after registration. A holder that lies
    //   about its identity can match the tuple even if it waits to reply.
    // Under that honest self-identity, request-then-reply condition:
    //   * peer already exited, pid not yet recycled -> `proc_find` does
    //     not return zombies -> `EV_ADD` is `ESRCH` -> `Undetermined`;
    //   * peer exited AND its pid was recycled, and no holder supplies a
    //     buffered or forged matching reply -> the exchange cannot prove
    //     it (EOF or deadline -> `Undetermined`);
    //   * an inheriting child honestly reports its own `(pid, pidversion)`
    //     -> that tuple differs from the observed one -> step 5 -> `Foreign`.
    // These conclusions do not cover a dishonest or pre-written reply.
    //
    // Residual, stated precisely: the request carries no nonce, and a
    // pidversion is not secret (`proc_pidinfo` with
    // `PROC_PIDUNIQIDENTIFIERINFO` can read another process's generation
    // without the same-user check, subject to other security-policy checks).
    // Whoever holds the peer's end can write a well-formed reply naming
    // an arbitrary `(pid, pidversion)`, before it is asked or after. Step 3
    // authorizes cached connection provenance, not the holder's current
    // euid: credential transitions or descriptor transfers leave it
    // unchanged (property 20). A forged reply cannot change that account
    // decision, but can misattribute a process across accounts. Process
    // attribution requires an honest responder reporting its own identity;
    // liveness after registration also requires reading the request first.
    let kq = match watch_exit(pid) {
        Ok(Some(kq)) => kq,
        // THIS caller's reading of `ESRCH` (`Ok(None)`): the peer is
        // nobody's child here, so nothing pins its number and an
        // unattachable pid proves only that it cannot be watched --
        // "unprovable", never "it exited". (`supervisor::probe::macos`, whose child
        // IS pinned by its own zombie, reads the same errno as proof of
        // exit; see [`watch_exit`].)
        Ok(None) => return ChallengeOutcome::Undetermined,
        // Nothing here says the peer is WRONG -- only that it cannot be
        // watched, and an unwatchable peer must not be minted as a
        // proof that carries a death signal.
        Err(_) => return ChallengeOutcome::Undetermined,
    };

    // Steps 4-5: the lane's own request/reply, the shared, platform-
    // neutral wire half -- see `crate::identity::challenge::exchange_identity`.
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
        kq,
        pid,
        created,
        exited: AtomicBool::new(false),
    })
}

/// ADR 0041 Lifecycle "The challenge", steps 1-3 ONLY: authenticate
/// cached connection provenance and return the live token's process
/// observation. This does not establish the holder's current euid or
/// bind it to that process. No wire I/O -- the lane-agnostic connect
/// constructor offers these checks, not the full challenge.
pub fn authenticate_server(conn: &dyn SocketChallengeable) -> PeerAuthOutcome {
    match authenticate_steps_1_to_3(conn) {
        ChallengeOutcome::Foreign => PeerAuthOutcome::Foreign,
        ChallengeOutcome::Undetermined => PeerAuthOutcome::Undetermined,
        ChallengeOutcome::Proven((pid, created)) => {
            PeerAuthOutcome::Authenticated(PeerAuthenticated { pid, created })
        }
    }
}
