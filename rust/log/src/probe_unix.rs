//! Linux half of the probe classifier's OS-facing seam (`crate::probe`
//! is the platform-neutral trait and scripted test support — see that
//! module's own doc): [`RealProbeOps`], the real `ProbeOps` implementation
//! over a real Unix domain socket and a real spawned child, and
//! [`SpawnedChild`], the owned, not-yet-challenged child handle Stage A's
//! A1-A3 observations are about. Mirrors `probe_win.rs` in shape; every
//! Win32 mechanism there has a pidfd-based replacement here (ADR 0043
//! decisions 8/21). No decision logic — just the mechanical OS calls the
//! classifier drives through [`ProbeOps`].

#![cfg(target_os = "linux")]

use crate::challenge::ChallengeOutcome;
use crate::challenge_unix::{self, ChallengedProcess};
use crate::probe::{ConnectOutcome, FenceProbe, ProbeOps, SpawnOutcome, WaitOutcome};
use std::cell::Cell;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::time::{Duration, Instant};

/// A just-spawned, NOT YET CHALLENGED child process handle. Stage A's
/// A1-A3 observations are about THIS type, never `ChallengedProcess`:
/// nothing has proven this handle's identity — it's ours only because we
/// just created it (mirrors `probe_win::SpawnedChild`'s own doc).
///
/// # Reaping (ADR 0043 decision 21)
///
/// `SIGCHLD` stays `SIG_DFL` for this whole process (the supervisor's own
/// module doc, `supervisor.rs`) — an ignored `SIGCHLD` would auto-reap
/// and re-open the pid-reuse window `pidfd_open` right after `spawn`
/// depends on staying closed. This type's own `pidfd` is what makes that
/// safe: the child stays a retained zombie, its pid unrecycled, until
/// [`Self::wait`] observes the exit and reaps it THEN — "reaps on the
/// exit it observes", the moment there is nothing further Stage A needs
/// to read off it (unlike a [`ChallengedProcess`], whose own reap is
/// deferred to `exit_status_after_confirmed_exit` because a caller may
/// still want to read its exit status first).
pub struct SpawnedChild {
    pid: libc::pid_t,
    pidfd: OwnedFd,
    /// The child's start time on `CLOCK_BOOTTIME` ticks, read from `/proc`
    /// ONCE, right after `spawn` — while the child is certainly still
    /// there (alive, or an unreaped zombie whose `/proc` entry persists) —
    /// so `identity()` never depends on the process still existing later,
    /// the way the Windows twin reads creation time from the handle.
    start_ticks: u64,
    reaped: Cell<bool>,
}

impl SpawnedChild {
    /// Wraps a freshly spawned [`std::process::Child`]: `pidfd_open`s it
    /// immediately (safe — `SIGCHLD` stays `SIG_DFL` and nothing else
    /// reaps this child before this call, so its pid cannot have been
    /// recycled out from under `pidfd_open` yet), then drops `child`
    /// itself — it holds nothing else this probe needs (no piped stdio
    /// was ever requested).
    fn from_child(child: std::process::Child) -> std::io::Result<Self> {
        let pid = child.id() as libc::pid_t;
        match challenge_unix::pidfd_open(pid as u32).and_then(|pidfd| {
            challenge_unix::process_start_ticks(pid as u32).map(|start_ticks| (pidfd, start_ticks))
        }) {
            Ok((pidfd, start_ticks)) => {
                drop(child);
                Ok(Self { pid, pidfd, start_ticks, reaped: Cell::new(false) })
            }
            Err(e) => {
                // Could not get a pidfd to track this child at all — an
                // extremely rare failure (e.g. `EMFILE`, or the child
                // already exited in the narrow window between `spawn`
                // and here). Best-effort: kill and reap it directly by
                // pid rather than leaking a live, wholly untracked
                // process, then report the ORIGINAL failure — the
                // classifier's own SPAWN-FAILED row already covers "we
                // could not obtain a usable handle for what we just
                // spawned".
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    let mut status: libc::c_int = 0;
                    libc::waitpid(pid, &mut status, 0);
                }
                Err(e)
            }
        }
    }

    /// See [`challenge_unix::poll_pidfd_readable`]'s own doc for the
    /// bound. Reaps (`waitid(P_PIDFD, ..)`, ADR 0043 decision 21) the
    /// MOMENT this observes the exit — see this type's own doc for why
    /// that is safe here, unlike [`ChallengedProcess`].
    pub fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        let exited = challenge_unix::poll_pidfd_readable(self.pidfd.as_raw_fd(), timeout)?;
        if exited {
            self.reap();
        }
        Ok(exited)
    }

    pub fn terminate(&self) -> std::io::Result<()> {
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
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// This CHILD's own `(pid, creation time)`, read independently of
    /// anything a challenge over its pipe observed (Codex review round 1,
    /// finding 10 — carried over verbatim from `probe_win::
    /// SpawnedChild::identity`'s own doc): the same start-time identity
    /// `challenge_unix` reports for a PEER.
    pub fn identity(&self) -> std::io::Result<(u32, u64)> {
        Ok((self.pid as u32, self.start_ticks))
    }

    /// Reap exactly once (`reaped` guards a second call from re-reaping
    /// whatever pid the kernel may since have recycled this one into) via
    /// `waitid(P_PIDFD, ..., WEXITED | WNOHANG)` — race-free against pid
    /// reuse because it targets the pidfd, never the numeric pid.
    /// Ignores its own outcome: this is a confirmed-exited child THIS
    /// process's own `SIGCHLD` disposition (`SIG_DFL`) left retained for
    /// exactly this call.
    fn reap(&self) {
        if self.reaped.replace(true) {
            return;
        }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::waitid(
                libc::P_PIDFD,
                self.pidfd.as_raw_fd() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG,
            );
        }
    }
}

/// The real implementation: an unchallenged voyage-socket connect,
/// `challenge_unix::challenge` (with the voyage mgmt lane's own
/// `VoyageMgmtExchange`), `std::process` spawn, and the bounded
/// wait/terminate helpers, unmediated. No decisions — just the
/// mechanical OS calls the classifier drives through [`ProbeOps`].
/// `pub(crate)`, not `pub` — mirrors `probe_win::RealProbeOps`'s own doc
/// for why (no production consumer outside this crate; `sot-capsule`
/// reaches this crate only through its `pub` API regardless). This
/// lane's own consumer is `supervisor.rs`, on Linux exactly as on
/// Windows.
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
        challenge_unix::challenge(conn, &mut exchange, deadline)
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

    fn proven_identity(&self, process: &Self::Process) -> (u32, u64) {
        (process.pid(), process.created())
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}
