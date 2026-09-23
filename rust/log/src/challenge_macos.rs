//! macOS half of the same-connection challenge (ADR 0043 decision 8):
//! ONE `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)` -- that single call IS
//! steps 1-3 -- plus the identity a proof returns
//! ([`ChallengedProcess`]). Steps 4-5 (the wire half) are shared,
//! platform-neutral logic in `crate::challenge` -- see that module's own
//! doc; [`challenge()`] and [`authenticate_server()`] below call into it
//! rather than reimplementing it. Mirrors `challenge_unix.rs` in SHAPE,
//! and is deliberately a fraction of its length -- why, is the whole
//! point of this module (see "What macOS does not need" below).
//!
//! # The kernel fact this rests on
//!
//! `tests/macos_kernel_facts.rs` pins it as a permanent, named
//! regression test rather than a one-off probe: on a connected `AF_UNIX`
//! socket, `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)` hands a CLIENT the
//! 32-byte `audit_token_t` of its peer -- the SERVER -- carrying that
//! peer's euid (word 1), its pid (word 5) and its `pidversion` (word 7),
//! the kernel's own monotonic process-creation generation. THE DIRECTION
//! IS WHAT MAKES [`authenticate_server()`] POSSIBLE AT ALL, exactly as
//! on the other two platforms: Linux latches `SO_PEERCRED` onto both
//! ends at `connect(2)`, Windows' `GetNamedPipeServerProcessId` resolves
//! the peer from either end, and here the client's OWN fd is where the
//! server's identity is read. A macOS kernel that changed this would
//! report itself as that named red test, not as a silent auth
//! regression.
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
//! window between them. Darwin's token is latched at `connect(2)` and
//! carries `(pid, pidversion)` TOGETHER, from one kernel read: there is
//! no second mechanism to reconcile, no anchor to sample, and no window
//! to narrow. Steps 1-3 are therefore one syscall and three comparisons,
//! and [`SocketChallengeable`] here has no `connect_anchor_boot_ticks`
//! twin -- `socket_unix::SocketClient`'s own anchor field stays unread
//! (and `#[allow(dead_code)]`) on this target. Adding a second identity
//! path here would be building a ladder the kernel already climbed.
//!
//! # `created` is the pidversion
//!
//! Every platform's `created` is "whatever unit this OS's own
//! `status_ok.created` carries, compared for equality only"
//! (`client::PeerIdentity::created`): Windows packs the creation
//! `FILETIME`, Linux the `/proc` start ticks, macOS the `pidversion`.
//! It is the right unit here precisely because it is the anti-reuse
//! generation itself -- a reply binding `(pid, pidversion)` binds the
//! process INSTANCE, not merely a number the kernel may hand out again.
//! [`self_pidversion()`] is the self-facing twin a server reports with,
//! the macOS counterpart of `challenge_unix::self_start_ticks`.
//!
//! # Scope: identity, not the death watch (milestone M2)
//!
//! [`ChallengedProcess`] here carries NO retained kernel handle, because
//! macOS has no pidfd and nothing else that keeps a process object
//! alive by reference. Its closest equivalent -- a `kqueue`
//! `EVFILT_PROC`/`NOTE_EXIT` registration, which attaches to the live
//! process instance and refuses (`ESRCH`) once it is gone -- is the next
//! milestone's work, and `reverify`/`wait`/`terminate` land WITH it, as
//! one mechanism. Until then this type deliberately offers identity
//! only: it implements [`crate::client::PeerIdentity`] (pid + created)
//! and NOT `PeerProcess`, so nothing can consume a macOS proof as though
//! it carried a death signal it does not have. That is also why
//! `socket_unix::SocketEndpoint` gains no macOS arm in this milestone --
//! its `Endpoint::Process` is required to be a full `PeerProcess`.

#![cfg(target_os = "macos")]

use crate::challenge::{
    exchange_identity, ChallengeOutcome, ChallengeableConnection, PeerAuthOutcome, PeerAuthenticated,
    StatusFailure,
};
use crate::client::PeerIdentity;
use crate::exchange::IdentityExchange;
use std::io;
use std::os::fd::RawFd;
use std::time::Instant;

/// The macOS-shaped extension every [`ChallengeableConnection`] this
/// crate actually challenges must also supply: raw fd access, for the
/// one `getsockopt` steps 1-3 consist of -- the macOS twin of
/// `challenge_win::PipeChallengeable` and `challenge_unix::
/// SocketChallengeable`. Deliberately ONE method: the Linux twin's
/// second one (`connect_anchor_boot_ticks`) exists only to anchor a pin
/// this platform gets from the kernel already (module doc, "What macOS
/// does not need").
pub trait SocketChallengeable: ChallengeableConnection {
    /// This end's own fd for the connected socket -- `LOCAL_PEERTOKEN`
    /// resolves the peer from either end (see this module's own doc), so
    /// the CLIENT's own fd is exactly what steps 1-3 need.
    fn raw_fd(&self) -> RawFd;
}

/// `audit_token_t` -- eight `u32`s, in the order Apple's
/// `audit_token_to_*` accessors define: auid, euid, egid, ruid, rgid,
/// pid, asid, pidversion. Declared here rather than taken from `libc`,
/// which exports the `SOL_LOCAL`/`LOCAL_PEERTOKEN` constants for apple
/// targets but not this struct -- the same local declaration
/// `tests/macos_kernel_facts.rs` already proved the shape of.
#[repr(C)]
#[derive(Clone, Copy)]
struct AuditToken {
    val: [u32; 8],
}

const TOK_EUID: usize = 1;
const TOK_PID: usize = 5;
const TOK_PIDVERSION: usize = 7;

/// The three words of the peer's audit token this module reads -- the
/// macOS twin of `challenge_unix::PeerCredentials`, and like it the
/// product of exactly ONE `getsockopt`. Private: unlike the Linux
/// version (whose `pin_peer_for_test` hook takes one), nothing outside
/// this module has a pin path to drive.
struct PeerToken {
    pid: u32,
    euid: u32,
    /// The kernel's process-creation generation for `pid`, latched into
    /// this token at `connect(2)`. Zero is treated as "no generation"
    /// and never trusted -- see [`authenticate_steps_1_to_3`].
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
        euid: token.val[TOK_EUID],
        pidversion: token.val[TOK_PIDVERSION],
    })
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
/// and [`authenticate_server()`]: one `getsockopt` for the peer's whole
/// audit token, the two "is this even an identity" rejections, then the
/// same-user comparison. Returns `(pid, created)`; `Foreign`/
/// `Undetermined` are already the caller's own terminal outcome.
fn authenticate_steps_1_to_3(conn: &dyn SocketChallengeable) -> ChallengeOutcome<(u32, u64)> {
    // Steps 1-2: one getsockopt for pid + euid + pidversion together.
    let token = match peer_token(conn.raw_fd()) {
        Ok(t) => t,
        Err(_) => return ChallengeOutcome::Undetermined,
    };
    if token.pid == 0 {
        // No peer pid at all -- never a real pid to bind a reply to
        // (the twin of `challenge_unix`'s own `creds.pid == 0` arm).
        return ChallengeOutcome::Undetermined;
    }
    if token.pidversion == 0 {
        // A pid with no generation behind it is exactly the identity
        // Linux refuses to proceed on without a pidfd: the number alone
        // cannot distinguish this process from its successor. Not
        // `Foreign` -- nothing here says the peer is WRONG, only that it
        // is unprovable (`tests/macos_kernel_facts.rs` pins a real peer
        // as having a non-zero one).
        return ChallengeOutcome::Undetermined;
    }

    // Step 3: nothing past this point trusts, decodes, or acts on
    // anything from the peer until same-user equality has been checked
    // (property 20).
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    if token.euid != unsafe { libc::geteuid() } {
        return ChallengeOutcome::Foreign;
    }

    ChallengeOutcome::Proven((token.pid, u64::from(token.pidversion)))
}

/// The identity of a process this crate has PROVEN is the server behind
/// one challenged connection: `(pid, pidversion)`, both latched into the
/// peer token at `connect(2)`. ONLY the full five-step [`challenge()`]
/// ever produces one -- see [`PeerAuthenticated`] for the deliberately
/// weaker steps-1-3-only counterpart, which differs from this type ONLY
/// in provenance today (it is bound to no reply), and will differ in
/// capability too once the death watch lands.
///
/// Mirrors `challenge_win::ChallengedProcess` and
/// `challenge_unix::ChallengedProcess` in name and shape, but carries no
/// retained handle and offers no `reverify`/`wait`/`terminate` yet --
/// see this module's own doc ("Scope") for why those three arrive
/// together with the `kqueue` death watch rather than one at a time.
#[derive(Clone, Copy)]
pub struct ChallengedProcess {
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
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The `pidversion` this identity was PROVEN against -- the unit the
    /// macOS wire's own `status_ok.created` carries (see the module doc,
    /// "`created` is the pidversion"), compared for equality only.
    pub fn created(&self) -> u64 {
        self.created
    }
}

/// L1-unix LU3a (ADR 0043 decision 19): the identity half of the seam.
/// The `PeerProcess` half is deliberately NOT implemented here -- see
/// the module doc's "Scope".
impl PeerIdentity for ChallengedProcess {
    fn pid(&self) -> u32 {
        ChallengedProcess::pid(self)
    }

    fn created(&self) -> u64 {
        ChallengedProcess::created(self)
    }
}

/// The five pinned steps (ADR 0041 Lifecycle "The challenge", ADR 0043
/// decision 8), in order: (1-2) `LOCAL_PEERTOKEN` for
/// `(pid, euid, pidversion)`; (3) same-user comparison; (4) only then
/// `exchange`'s request on the SAME connection; (5) proven iff same-user
/// matched AND reply-pid == the observed pid AND reply creation == the
/// observed pidversion -- pid compared FIRST (mirrors both siblings'
/// ordering: a provably wrong pid must ALWAYS be `Foreign`, never
/// `Undetermined` because something else also failed). Nothing in the
/// reply is decoded for meaning or acted on before step 3 succeeds.
///
/// `reply_deadline` bounds ONLY steps 4-5 -- steps 1-3 are one local,
/// synchronous OS call with no wait to bound. `Proven` here means the
/// FULL five-step proof -- see [`authenticate_server()`] for the
/// deliberately separate, deliberately weaker steps-1-3-only operation.
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

    // Steps 4-5: the lane's own request/reply, the shared, platform-
    // neutral wire half -- see `crate::challenge::exchange_identity`.
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

    ChallengeOutcome::Proven(ChallengedProcess { pid, created })
}

/// ADR 0041 Lifecycle "The challenge", steps 1-3 ONLY: identify the peer
/// process behind a live connection and authenticate its same-user
/// identity. No wire I/O of any kind -- see `challenge_win::
/// authenticate_server`'s own doc for why the shared, lane-agnostic
/// connect constructor can only ever offer this, never the full proof.
pub fn authenticate_server(conn: &dyn SocketChallengeable) -> PeerAuthOutcome {
    match authenticate_steps_1_to_3(conn) {
        ChallengeOutcome::Foreign => PeerAuthOutcome::Foreign,
        ChallengeOutcome::Undetermined => PeerAuthOutcome::Undetermined,
        ChallengeOutcome::Proven((pid, created)) => {
            PeerAuthOutcome::Authenticated(PeerAuthenticated { pid, created })
        }
    }
}
