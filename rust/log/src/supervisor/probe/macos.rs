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
//! # The kqueue mechanism itself lives in `challenge_macos`
//!
//! Arming the knote, draining it, and the `Duration` -> `timespec`
//! conversion `kevent(2)` takes BY POINTER (a null timeout means BLOCK
//! FOREVER, and the supervisor polls with `Duration::ZERO` on every
//! `Ready` tick) are `challenge_macos::watch_exit`/`drain_exit`,
//! consumed here rather than written a second time — the direction this
//! module already runs in everywhere else (`probe_unix` consumes
//! `challenge_unix`), and the one place the zero-timeout mistake can be
//! made is therefore also the one place a test pins it.
//!
//! What stays HERE is the policy that primitive deliberately refuses to
//! decide: an `ESRCH` at attach time is proof of exit for THIS owned,
//! zombie-pinned child, where for a peer nobody spawned it is merely
//! unprovable.

#![cfg(target_os = "macos")]

use crate::challenge::ChallengeOutcome;
use crate::challenge_macos::{drain_exit, watch_exit, ChallengedProcess};
use crate::probe::{ConnectOutcome, FenceProbe, ProbeOps, SpawnOutcome, WaitOutcome};
use std::cell::Cell;
use std::os::fd::{AsRawFd, OwnedFd};
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
    /// [`watch_exit`]).
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
        match watch_exit(pid as u32) {
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
            Some(kq) => drain_exit(kq.as_raw_fd(), timeout)?,
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

