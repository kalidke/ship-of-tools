//! macOS half of the probe classifier's OS-facing seam (`crate::probe`
//! is the platform-neutral trait and scripted test support — see that
//! module's own doc): [`RealProbeOps`], the real `ProbeOps`
//! implementation over a real Unix domain socket and a real spawned
//! child, and [`SpawnedChild`], the owned, not-yet-challenged child
//! handle Stage A's A1-A3 observations are about. Mirrors
//! `probe_unix.rs` in shape — but NOT by widening it: its every
//! mechanism is a pidfd (`SYS_pidfd_send_signal`, `P_PIDFD`,
//! `pidfd_open`), and none of those exist on Darwin.
//!
//! # The pidfd's replacement, and where it is weaker
//!
//! A pidfd is a kernel object that names a process INSTANCE, so Linux
//! gets three things from one handle: a death signal that cannot be
//! confused with a successor's (`poll`), a race-free kill
//! (`pidfd_send_signal`), and a race-free reap (`waitid(P_PIDFD)`).
//! macOS has one object with the first property and none with the other
//! two:
//!
//! - **Death signal — as strong.** A `kqueue` `EVFILT_PROC`/`NOTE_EXIT`
//!   knote attaches to the `proc` itself, so a recycled pid can never
//!   produce this handle's exit event. One kqueue fd per child, owned,
//!   closed by drop — the same one-handle-one-kernel-object shape. A
//!   kqueue fd is not inherited across `fork(2)` at all, so unlike a
//!   pipe it needs no `CLOEXEC` dance and can never leak into a leg.
//! - **Kill — weaker, and bounded rather than proven.** There is no
//!   `pidfd_send_signal` twin (`task_for_pid` on anyone else is
//!   entitlement-gated), so [`SpawnedChild::terminate`] is
//!   `kill(pid, SIGKILL)` by number. For THIS type that is exactly as
//!   safe as the Linux call, because the pid is pinned: `SIGCHLD` is
//!   `SIG_DFL` for the supervisor's whole life (`supervisor::
//!   supervise_inner`, F2) and nothing auto-reaps, so an exited child is
//!   a retained zombie whose number cannot be recycled. `probe_unix::
//!   SpawnedChild::from_child`'s own failure-path `libc::kill` already
//!   rests on precisely this premise. The pin ends at the reap, which is
//!   why [`SpawnedChild::reap`] latches and [`SpawnedChild::terminate`]
//!   refuses after it.
//! - **Reap — weaker, same way.** `waitpid(pid, WNOHANG)` by number, and
//!   safe for the same reason: it is only ever called after THIS
//!   handle's own exit observation, while the zombie still pins the
//!   number.
//!
//! # `NOTE_EXIT` is delivered once — the latch is load-bearing
//!
//! A pidfd stays readable forever once the process exits; the kernel
//! supplies that stickiness for free. A knote does not: `NOTE_EXIT` is
//! delivered once and the knote is then detached, so a second `wait`
//! after a successful one would block for the whole timeout and return
//! `false` — the exact inversion of the truth. [`SpawnedChild::exited`]
//! is what supplies the stickiness instead, and every entry point reads
//! it first.
//!
//! # `Duration::ZERO` is `timespec { 0, 0 }`, never a null timeout
//!
//! A null `timeout` to `kevent(2)` means BLOCK FOREVER. The supervisor
//! polls with `Duration::ZERO` on every `Ready` tick, so the one
//! mistake that would hang its main loop is spelled as an omission
//! rather than as a wrong value — [`timeout_to_timespec`] exists so
//! that pointer is never null, and its own test pins the zero case.

#![cfg(target_os = "macos")]

use crate::challenge::ChallengeOutcome;
use crate::challenge_macos::ChallengedProcess;
use crate::probe::{ConnectOutcome, FenceProbe, ProbeOps, SpawnOutcome, WaitOutcome};
use std::cell::Cell;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::time::{Duration, Instant};

/// Bounds [`SpawnedChild::from_child`]'s own failure-path reap — the
/// same bound, for the same reason, as `probe_unix`'s own
/// `FAILURE_CLEANUP_REAP_BOUND` (a leader stuck in an uninterruptible
/// kernel wait must never block the caller forever).
const FAILURE_CLEANUP_REAP_BOUND: Duration = Duration::from_secs(2);

/// What BOTH halves of A4's identity comparison put in the generation
/// slot on this platform, so that comparison is pid equality and says so.
///
/// Every other platform has a second field it can read INDEPENDENTLY of
/// the challenge: Windows reads the creation `FILETIME` off the child's
/// own `HANDLE`, Linux the start ticks out of `/proc`. macOS's identity
/// unit is the kernel's `pidversion` (`challenge_macos`'s module doc,
/// "`created` is the pidversion"), and there is no user-space API that
/// reads another process's — not `proc_bsdinfo`, not
/// `proc_bsdshortinfo`, and `task_for_pid` needs an entitlement. That is
/// true of a CHILD too: the only source is the audit token the kernel
/// latches onto a socket at `connect(2)`, which is the challenge's own
/// source. Reporting it here would therefore compare the challenge
/// against itself.
///
/// What A4 actually needs is still decided, by pid alone, because the
/// child's pid is PINNED: `SIGCHLD` is `SIG_DFL` and nothing reaps this
/// child before [`SpawnedChild::wait`] observes its exit, so at the
/// moment of the comparison the number still names our child and nothing
/// else. The peer's pid comes from the kernel's own audit token, not
/// from the peer. Equal pids therefore mean the answering server IS this
/// episode's child, which is exactly the question A4 asks — a stale,
/// orphaned capsule from a prior crash holds its own, different number.
///
/// This is narrower than the two siblings, which compare a second field
/// as well, and it is stated here rather than hidden in a `0`: if a
/// future macOS ever exposes a peer generation, this constant is the one
/// place both halves stop using it.
const NO_OWNER_READABLE_GENERATION: u64 = 0;

/// `Duration` -> `timespec`, saturating rather than wrapping at both
/// ends, mirroring `challenge_unix::poll_pidfd_readable`'s own
/// `i32::try_from(millis).unwrap_or(i32::MAX)`. Its whole reason for
/// existing is that the result is always a real struct a caller passes
/// BY POINTER — see this module's own doc on the null timeout.
fn timeout_to_timespec(timeout: Duration) -> libc::timespec {
    match libc::time_t::try_from(timeout.as_secs()) {
        Ok(tv_sec) => libc::timespec { tv_sec, tv_nsec: libc::c_long::from(timeout.subsec_nanos()) },
        // Past `time_t`'s range there is no finer part left to carry.
        Err(_) => libc::timespec { tv_sec: libc::time_t::MAX, tv_nsec: 0 },
    }
}

/// Arm a fresh `kqueue` with an `EVFILT_PROC`/`NOTE_EXIT` knote on `pid`.
///
/// `Ok(None)` is `ESRCH` — the kernel's `proc_find` does not return
/// zombies, so "not attachable" is what an already-exited process looks
/// like here. WHO OWNS THE PID DECIDES WHAT THAT MEANS, and this
/// function deliberately does not decide: for an owned child (the only
/// caller in this module) the pid is pinned by the zombie, so it
/// provably means *already exited*; for a peer nobody spawned it would
/// mean *unprovable*. Same errno, two correct readings, and the type
/// that receives it encodes the ownership.
///
/// `EV_ADD | EV_RECEIPT` with a one-entry eventlist, so an attach
/// failure arrives deterministically as an `EV_ERROR` event carrying the
/// errno in `data` rather than as a `kevent` return of `-1`. No
/// `EV_ONESHOT` and no `EV_CLEAR`: neither names an invariant here, and
/// the caller's own latch already owns the once-only semantics.
fn exit_watch_open(pid: libc::pid_t) -> std::io::Result<Option<OwnedFd>> {
    // SAFETY: `kqueue` takes no arguments and returns a new fd or -1.
    let raw = unsafe { libc::kqueue() };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Wrapped immediately so every early return below closes it. No
    // `FD_CLOEXEC` dance: a kqueue fd is not inherited across `fork(2)`
    // at all, so there is no window in which a leg could hold one.
    // SAFETY: `raw` was just returned by `kqueue` and is owned by nobody
    // else.
    let kq = unsafe { OwnedFd::from_raw_fd(raw) };

    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let mut out: libc::kevent = unsafe { std::mem::zeroed() };
    let ts = timeout_to_timespec(Duration::ZERO);
    // SAFETY: `kq` is live for the whole call; `change` and `out` are
    // one real `kevent` each and the counts say so; `ts` is a real
    // `timespec` (a null pointer here would mean "block forever").
    let rc = unsafe { libc::kevent(kq.as_raw_fd(), &change, 1, &mut out, 1, &ts) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if rc == 0 {
        // `EV_RECEIPT` guarantees exactly one receipt event per change,
        // so this cannot happen; refuse rather than return a kqueue
        // whose knote was never confirmed.
        return Err(std::io::Error::other("kevent(EV_RECEIPT) returned no receipt"));
    }
    if out.flags & libc::EV_ERROR != 0 {
        return match out.data as libc::c_int {
            0 => Ok(Some(kq)), // `EV_RECEIPT`'s own success receipt
            libc::ESRCH => Ok(None),
            e => Err(std::io::Error::from_raw_os_error(e)),
        };
    }
    Ok(Some(kq))
}

/// Has this watch's process exited, within `timeout`? The twin of
/// `challenge_unix::poll_pidfd_readable`, bound for bound: `EINTR` is
/// an `Err` here exactly as it is there (`ProbeOps::wait_exit` maps
/// `Err` to `WaitFailed`), rather than silently improving one platform.
///
/// Delivers at most once — the caller's latch is what makes a second
/// call honest. See this module's own doc.
fn poll_exit_watch(kq: RawFd, timeout: Duration) -> std::io::Result<bool> {
    let mut out: libc::kevent = unsafe { std::mem::zeroed() };
    let ts = timeout_to_timespec(timeout);
    // SAFETY: `kq` is owned by the caller for the whole call; `out` is
    // one real `kevent` and the count says so; `ts` is a real
    // `timespec`, never null (see this module's doc).
    let rc = unsafe { libc::kevent(kq, std::ptr::null(), 0, &mut out, 1, &ts) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if rc == 0 {
        return Ok(false); // the timeout expired with no exit
    }
    if out.flags & libc::EV_ERROR != 0 {
        return match out.data as libc::c_int {
            // The knote's process went away without the event this
            // handle was waiting for. It is gone either way, and that
            // is the answer being asked for.
            libc::ESRCH => Ok(true),
            e => Err(std::io::Error::from_raw_os_error(e)),
        };
    }
    Ok(out.fflags & libc::NOTE_EXIT != 0)
}

/// A just-spawned, NOT YET CHALLENGED child process handle — the macOS
/// twin of `probe_unix::SpawnedChild`, and identical in contract: Stage
/// A's A1-A3 observations are about THIS type, never
/// [`ChallengedProcess`], because nothing has proven this handle's
/// identity — it is ours only because we just created it.
///
/// Reaps on the exit it observes (ADR 0043 decision 21), for the same
/// reason the Linux twin does: at that moment there is nothing further
/// Stage A needs to read off it. A [`ChallengedProcess`]'s own reap
/// stays a separate, explicit, owner-called step because a caller may
/// still want its exit status first.
pub struct SpawnedChild {
    pid: libc::pid_t,
    /// The exit watch, or `None` when the child was already gone at
    /// attach time — in which case `exited` starts latched, which is the
    /// only reading `ESRCH` can have for a pid a zombie pins (see
    /// [`exit_watch_open`]).
    watch: Option<OwnedFd>,
    /// The stickiness a pidfd gets from the kernel and a knote does not
    /// — see this module's own doc. `Cell`, not an atomic, matching the
    /// Linux twin: this handle is owned by the one thread that spawned
    /// it and never enters shared state.
    exited: Cell<bool>,
    reaped: Cell<bool>,
}

impl SpawnedChild {
    /// Wraps a freshly spawned [`std::process::Child`]: arms the exit
    /// watch immediately, then drops `child` itself — it holds nothing
    /// else this probe needs (no piped stdio was ever requested), and
    /// its `Drop` does not reap.
    ///
    /// Unlike the Linux twin, an already-exited child is NOT a failure
    /// here: `pidfd_open` succeeds on a zombie and `kqueue` attach does
    /// not, so `ESRCH` is the ordinary "it exited in the window between
    /// `spawn` and here" answer, and the handle is constructed already
    /// latched rather than discarded. Only a REAL failure to arm the
    /// watch (`EMFILE`, say) takes the kill-and-reap path, and it
    /// reports the original error — the classifier's SPAWN-FAILED row
    /// already covers "we could not obtain a usable handle for what we
    /// just spawned".
    fn from_child(child: std::process::Child) -> std::io::Result<Self> {
        let pid = child.id() as libc::pid_t;
        match exit_watch_open(pid) {
            Ok(watch) => {
                let already_exited = watch.is_none();
                drop(child);
                Ok(Self { pid, watch, exited: Cell::new(already_exited), reaped: Cell::new(false) })
            }
            Err(e) => {
                // Best-effort: kill and reap directly by pid rather than
                // leaking a live, wholly untracked process, then report
                // the ORIGINAL failure. The numeric kill is safe (F2
                // guarantees `SIGCHLD` is `SIG_DFL` here, so this pid is
                // still pinned, unrecycled), but the REAP that follows
                // must never block unboundedly: a leader stuck in an
                // uninterruptible kernel wait can outlive `SIGKILL`
                // entirely, and this runs on the classifier's own spawn
                // worker thread, not a destructor with nothing else
                // waiting on it. Verbatim the Linux twin's own bounded
                // loop — poll `waitpid(pid, WNOHANG)` every 10ms,
                // retrying `EINTR`, stopping on a real reap or `ECHILD`,
                // bounded by `FAILURE_CLEANUP_REAP_BOUND`. Past that
                // bound this simply gives up: a pinned zombie left
                // behind is harmless (nothing else addresses this pid),
                // and the ORIGINAL error is what matters to the caller.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                let deadline = Instant::now() + FAILURE_CLEANUP_REAP_BOUND;
                loop {
                    let mut status: libc::c_int = 0;
                    let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                    if rc == pid {
                        break; // reaped
                    }
                    if rc < 0 {
                        match std::io::Error::last_os_error().raw_os_error() {
                            Some(libc::EINTR) => continue, // retry immediately
                            _ => break, // ECHILD (already reaped) or anything else -- nothing more to do
                        }
                    }
                    // rc == 0: not yet reapable -- keep polling until the bound.
                    if Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e)
            }
        }
    }

    /// Reaps the MOMENT this observes the exit — see this type's own doc
    /// for why that is safe here, unlike [`ChallengedProcess`]. Reads
    /// the latch first, because `NOTE_EXIT` is delivered once (module
    /// doc) and a second `kevent` would otherwise burn the whole timeout
    /// and answer `false`.
    pub fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        if self.exited.get() {
            self.reap();
            return Ok(true);
        }
        let exited = match self.watch.as_ref() {
            Some(kq) => poll_exit_watch(kq.as_raw_fd(), timeout)?,
            // Unreachable: a handle with no watch was constructed
            // already latched, and the latch is never cleared.
            None => true,
        };
        if exited {
            self.exited.set(true);
            self.reap();
        }
        Ok(exited)
    }

    /// `kill(pid, SIGKILL)` by number — macOS has no `pidfd_send_signal`
    /// twin. Safe for this type because the pid is pinned by the zombie
    /// until the reap; REFUSED after it, since past that point the
    /// number is the kernel's to hand out again and this handle has no
    /// claim on whoever holds it. `Ok(())` means the signal was sent and
    /// nothing more: the proof of death is always the WAIT (the
    /// classifier's own KILL+WAIT row), never the kill.
    pub fn terminate(&self) -> std::io::Result<()> {
        if self.reaped.get() {
            return Ok(());
        }
        // SAFETY: a plain signal send; `pid` is this handle's own child.
        if unsafe { libc::kill(self.pid, libc::SIGKILL) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// This CHILD's own identity, read independently of anything a
    /// challenge over its socket observed — which on macOS is the pid
    /// and nothing else. See [`NO_OWNER_READABLE_GENERATION`] for why
    /// that is the honest pair here and what still makes A4 decidable.
    pub fn identity(&self) -> std::io::Result<(u32, u64)> {
        Ok((self.pid as u32, NO_OWNER_READABLE_GENERATION))
    }

    /// Reap exactly once (`reaped` guards a second call from re-reaping
    /// whatever pid the kernel may since have recycled this one into),
    /// and ONLY after this handle's own exit observation — that is what
    /// makes the numeric `waitpid` safe where Linux's `waitid(P_PIDFD)`
    /// is safe by construction. Ignores its own outcome: this is a
    /// confirmed-exited child THIS process's own `SIGCHLD` disposition
    /// (`SIG_DFL`) left retained for exactly this call, and `ECHILD`
    /// (someone else's to reap) is harmless.
    fn reap(&self) {
        if self.reaped.replace(true) {
            return;
        }
        let mut status: libc::c_int = 0;
        unsafe {
            libc::waitpid(self.pid, &mut status, libc::WNOHANG);
        }
    }
}

/// The real implementation: an unchallenged voyage-socket connect,
/// `challenge_macos::challenge` (with the voyage mgmt lane's own
/// `VoyageMgmtExchange`), `std::process` spawn, and the bounded
/// wait/terminate helpers, unmediated. No decisions — just the
/// mechanical OS calls the classifier drives through [`ProbeOps`].
/// `pub(crate)`, matching both siblings: no production consumer outside
/// this crate. This module's own consumer is `supervisor.rs`, on macOS
/// exactly as on Linux and Windows.
pub(crate) struct RealProbeOps;

impl ProbeOps for RealProbeOps {
    type Conn = crate::socket_unix::SocketClient;
    type SpawnedChild = SpawnedChild;
    type Process = ChallengedProcess;

    fn spawn(&self, command: &mut std::process::Command) -> SpawnOutcome<Self::SpawnedChild> {
        match command.spawn() {
            Ok(child) => match SpawnedChild::from_child(child) {
                Ok(sc) => SpawnOutcome::Spawned(sc),
                Err(e) => SpawnOutcome::Failed(e),
            },
            Err(e) => SpawnOutcome::Failed(e),
        }
    }

    fn wait_child(&self, child: &Self::SpawnedChild, timeout: Duration) -> WaitOutcome {
        match child.wait(timeout) {
            Ok(true) => WaitOutcome::Exited,
            Ok(false) => WaitOutcome::StillRunning,
            Err(_) => WaitOutcome::WaitFailed,
        }
    }

    fn kill_child(&self, child: &Self::SpawnedChild) -> std::io::Result<()> {
        child.terminate()
    }

    fn connect(&self, voyage_id: &str) -> ConnectOutcome<Self::Conn> {
        match crate::socket_unix::connect_voyage_socket_unchallenged(voyage_id) {
            Ok(client) => ConnectOutcome::Connected(client),
            Err(crate::transport::TransportError::Io { source, .. }) => match source.kind() {
                std::io::ErrorKind::NotFound => ConnectOutcome::FileNotFound,
                // A Unix domain socket with no listener REFUSES
                // (`ECONNREFUSED`) where a Windows named pipe that does
                // not exist is simply NOT FOUND — both mean the exact
                // same thing for the A/B table's own purposes ("the leg
                // is not there yet"), so both fold to `FileNotFound`.
                std::io::ErrorKind::ConnectionRefused => ConnectOutcome::FileNotFound,
                std::io::ErrorKind::PermissionDenied => ConnectOutcome::AccessDenied,
                // No `PipeBusy` on Unix: the kernel QUEUES a pending
                // connect against the listen backlog rather than
                // refusing it outright, so an `EAGAIN` that survives the
                // connect's own bounded retry surfaces as an ordinary,
                // unclassified `WouldBlock` here, folded into `OtherIo`
                // like any other one.
                _ => ConnectOutcome::OtherIo(source),
            },
            Err(other) => ConnectOutcome::OtherIo(std::io::Error::other(other)),
        }
    }

    fn challenge(&self, conn: &Self::Conn, deadline: Instant) -> ChallengeOutcome<Self::Process> {
        let mut exchange = crate::exchange::VoyageMgmtExchange::default();
        crate::challenge_macos::challenge(conn, &mut exchange, deadline)
    }

    fn writer_fence_probe(&self, voyage_root: &Path) -> FenceProbe {
        let lock_path = voyage_root.join("writer.lock");
        match crate::fsutil::lock_writer(&lock_path) {
            // The guard drops here, releasing the fence immediately --
            // this is a PROBE, never a hold.
            Ok(_guard) => FenceProbe::Free,
            Err(crate::Error::State(_)) => FenceProbe::Held,
            Err(crate::Error::Io(e)) => FenceProbe::Error(e),
            Err(other) => FenceProbe::Error(std::io::Error::other(other)),
        }
    }

    fn wait_exit(&self, process: &Self::Process, timeout: Duration) -> WaitOutcome {
        match process.wait(timeout) {
            Ok(true) => WaitOutcome::Exited,
            Ok(false) => WaitOutcome::StillRunning,
            Err(_) => WaitOutcome::WaitFailed,
        }
    }

    fn terminate(&self, process: &Self::Process) -> std::io::Result<()> {
        process.terminate()
    }

    fn spawned_identity(&self, child: &Self::SpawnedChild) -> std::io::Result<(u32, u64)> {
        child.identity()
    }

    /// The PROVEN peer's own half of A4's comparison. Its generation
    /// slot is [`NO_OWNER_READABLE_GENERATION`] for the reason stated
    /// there — the child half has no independently-readable generation
    /// to compare against, so reporting the real `created()` here would
    /// only guarantee the comparison never holds.
    /// [`ChallengedProcess::created`] itself is untouched and still
    /// carries the pidversion the challenge proved.
    fn proven_identity(&self, process: &Self::Process) -> (u32, u64) {
        (process.pid(), NO_OWNER_READABLE_GENERATION)
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one mistake in this module that would type-check, pass
    /// review, and hang the supervisor's main loop on its first `Ready`
    /// tick is a null `kevent` timeout. `Duration::ZERO` must therefore
    /// be a REAL all-zero `timespec`, and both saturating ends must
    /// stay finite.
    #[test]
    fn zero_timeout_is_a_zero_timespec_not_a_null_one() {
        let zero = timeout_to_timespec(Duration::ZERO);
        assert_eq!(zero.tv_sec, 0);
        assert_eq!(zero.tv_nsec, 0);

        let ms = timeout_to_timespec(Duration::from_millis(1500));
        assert_eq!(ms.tv_sec, 1);
        assert_eq!(ms.tv_nsec, 500_000_000);

        let huge = timeout_to_timespec(Duration::new(u64::MAX, 999_999_999));
        assert_eq!(huge.tv_sec, libc::time_t::MAX);
        assert_eq!(huge.tv_nsec, 0);
    }
}
