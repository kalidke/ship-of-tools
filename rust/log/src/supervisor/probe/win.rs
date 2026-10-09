//! Windows half of the probe classifier's OS-facing seam (`crate::supervisor::probe`
//! is the platform-neutral trait and scripted test support — see that
//! module's own doc): [`RealProbeOps`], the real `ProbeOps` implementation
//! over an actual named pipe and spawned child, and [`SpawnedChild`], the
//! owned, not-yet-challenged child handle Stage A's A1-A3 observations are
//! about. No decision logic — just the mechanical OS calls the classifier
//! drives through `ProbeOps`.

#![cfg(windows)]

use crate::identity::challenge::ChallengeOutcome;
use crate::capsule::producer::ExitStatus;
use crate::identity::challenge_win;
use crate::supervisor::probe::leg_process::LegProcess;
use crate::supervisor::probe::{ConnectOutcome, FenceProbe, ProbeOps, SpawnOutcome, WaitOutcome};
use crate::store::voyage::WRITER_LOCK;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::time::{Duration, Instant};

/// A just-spawned, NOT YET CHALLENGED child process handle. Stage A's
/// A1-A3 observations are about THIS type, never `ChallengedProcess`:
/// nothing has proven this handle's identity (it's ours only because we
/// just created it), so it carries none of the SID/creation-time
/// provenance a `ChallengedProcess` does — just enough to wait on it or
/// kill it. Dropping this closes the handle.
#[derive(Debug)]
pub struct SpawnedChild {
    handle: OwnedHandle,
    /// The exit code `GetExitCodeProcess` gave once `wait` had seen the
    /// process exit; `None` until then.
    exit_status: std::cell::Cell<Option<ExitStatus>>,
}

impl SpawnedChild {
    fn from_child(child: std::process::Child) -> Self {
        use std::os::windows::io::IntoRawHandle;
        // `into_raw_handle` consumes `child`, transferring ownership of
        // the PROCESS handle to us; any piped stdio the caller requested
        // are separate handles, dropped normally as `child`'s other
        // fields go out of scope here — untouched by this handoff.
        let raw = child.into_raw_handle();
        // SAFETY: `raw` came from `IntoRawHandle::into_raw_handle`,
        // which transfers unique ownership.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        Self { handle, exit_status: std::cell::Cell::new(None) }
    }

    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.handle.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE
    }

    /// See [`challenge_win::wait_handle`]'s doc for the bound.
    pub fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        let exited = challenge_win::wait_handle(self.raw(), timeout)?;
        if exited && self.exit_status.get().is_none() {
            use windows_sys::Win32::System::Threading::GetExitCodeProcess;
            let mut code = 0u32;
            if unsafe { GetExitCodeProcess(self.raw(), &mut code) } != 0 {
                self.exit_status.set(Some(ExitStatus::Code(code)));
            }
        }
        Ok(exited)
    }

    /// How the process ended, once [`Self::wait`] has seen it exit; `None`
    /// before that.
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit_status.get()
    }

    pub fn terminate(&self) -> std::io::Result<()> {
        challenge_win::terminate_handle(self.raw())
    }

    /// This CHILD's own `(pid, creation time)`, read directly off the
    /// handle THIS episode spawned — independent of anything a challenge
    /// over its pipe observed. A4's
    /// own transition ("alive, within cutoff, challenge proves it") never
    /// compared the challenged server's identity against the child that
    /// was actually spawned: a stale, orphaned capsule left over from a
    /// prior crash, still bound under the SAME voyage id while this new
    /// child's own pipe hadn't come up yet, would answer the challenge
    /// first and be accepted as if it WERE the freshly spawned leg. This
    /// is the independent half of that comparison; `classify::probe_owned_spawn`
    /// is the caller that actually compares it against the challenge's
    /// own `ChallengedProcess::pid`/`created`.
    pub fn identity(&self) -> std::io::Result<(u32, u64)> {
        use windows_sys::Win32::System::Threading::GetProcessId;
        let pid = unsafe { GetProcessId(self.raw()) };
        if pid == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let created = challenge_win::creation_filetime_bits(self.raw())?;
        Ok((pid, created))
    }
}

/// The real implementation: `connect_voyage_pipe_unchallenged`,
/// `challenge_win::challenge` (with the voyage mgmt lane's
/// `VoyageMgmtExchange`), `std::process` spawn, and the bounded
/// wait/terminate helpers, unmediated. No decisions — just the mechanical
/// OS calls the classifier drives through [`ProbeOps`]. `connect` uses the
/// UNCHALLENGED connect (`pipe_win::connect_voyage_pipe` itself runs SID
/// authentication internally, which would collapse Stage B's own
/// connect-then-challenge rows — see that function's doc) — it currently
/// carries that raw connect's existing ~2s internal retry on
/// `PIPE_BUSY`/`FILE_NOT_FOUND`; U0 does not change that function's
/// behavior, so this is what's available today (a later unit may want a
/// non-retrying variant for the classifier's own 500ms-spaced probe loop).
///
/// # `pub(crate)`, not `pub`
///
/// An earlier version of this type was `pub`, which made the UNCHALLENGED
/// connection it hands back through `ConnectOutcome::Connected` a public
/// path to raw pipe I/O: external code could call
/// `RealProbeOps.connect(id)`, extract the `PipeClient`, and read/write it
/// directly without ever running `challenge`/`authenticate_server` — the
/// exact leak `connect_voyage_pipe`'s own enforcement exists to close,
/// reopened one layer up. `RealProbeOps` has no production consumer today
/// (this whole module is scaffolding — see the module doc), so nothing
/// depends on it being public, and `sot-capsule` (a separate bin CRATE
/// TARGET, `src/bin/sot-capsule.rs`) cannot see `pub(crate)` items
/// regardless: it only ever reaches this library through its `pub` API.
/// That is the intended shape once U2's classifier lands — a `pub`
/// function/type living IN THIS CRATE, wrapping `RealProbeOps` internally,
/// which `sot-capsule` calls — never `RealProbeOps` directly. Keeping the
/// trait (`ProbeOps`) and the scripted implementation (`ScriptedProbeOps`,
/// already gated behind `cfg(test)`/`test-support`) at their current
/// visibility is unaffected: neither one hands back a real, unauthenticated
/// `PipeClient` to anything outside this crate. `sot-capsule
/// supervise` (`supervisor/`) is that consumer, on Windows exactly as
/// on Linux.
pub(crate) struct RealProbeOps;

impl ProbeOps for RealProbeOps {
    type Conn = crate::lane::pipe_win::PipeClient;
    type SpawnedChild = SpawnedChild;
    type Process = LegProcess;

    fn spawn(&self, command: &mut std::process::Command) -> SpawnOutcome<Self::SpawnedChild> {
        #[allow(clippy::disallowed_methods, reason = "the supervisor starts its leg (ADR 0043)")]
        let spawned = command.spawn();
        match spawned {
            Ok(child) => SpawnOutcome::Spawned(SpawnedChild::from_child(child)),
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
        match crate::lane::pipe_win::connect_voyage_pipe_unchallenged(voyage_id) {
            Ok(client) => ConnectOutcome::Connected(client),
            Err(crate::lane::transport::TransportError::Io { source, .. }) => {
                use windows_sys::Win32::Foundation::{
                    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY,
                };
                match source.raw_os_error() {
                    Some(c) if c == ERROR_FILE_NOT_FOUND as i32 => ConnectOutcome::FileNotFound,
                    Some(c) if c == ERROR_PIPE_BUSY as i32 => ConnectOutcome::PipeBusy,
                    Some(c) if c == ERROR_ACCESS_DENIED as i32 => ConnectOutcome::AccessDenied,
                    _ => ConnectOutcome::OtherIo(source),
                }
            }
            Err(other) => ConnectOutcome::OtherIo(std::io::Error::other(other)),
        }
    }

    fn challenge(&self, conn: &Self::Conn, deadline: Instant) -> ChallengeOutcome<Self::Process> {
        let mut exchange = crate::identity::exchange::VoyageMgmtExchange::default();
        match challenge_win::challenge(conn, &mut exchange, deadline) {
            ChallengeOutcome::Proven(process) => ChallengeOutcome::Proven(LegProcess::Adopted(process)),
            ChallengeOutcome::Foreign => ChallengeOutcome::Foreign,
            ChallengeOutcome::Undetermined => ChallengeOutcome::Undetermined,
        }
    }

    fn writer_fence_probe(&self, voyage_root: &Path) -> FenceProbe {
        let lock_path = voyage_root.join(WRITER_LOCK);
        match crate::host::lock_writer(&lock_path) {
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
        process.identity()
    }

    fn child_exit_status(&self, child: &Self::SpawnedChild) -> Option<ExitStatus> {
        child.exit_status()
    }

    /// A4 has proved the answering server is this child, so the child itself
    /// is what the supervisor keeps.
    fn retain_owned_child(&self, _proven: Self::Process, child: Self::SpawnedChild) -> Self::Process {
        LegProcess::Owned(child)
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawns `cmd /c exit N` through `RealProbeOps::spawn` and waits,
    /// bounded, for it to exit.
    fn ended(code: u32) -> LegProcess {
        let mut command = std::process::Command::new("cmd.exe");
        command.args(["/d", "/c", &format!("exit {code}")]);
        let SpawnOutcome::Spawned(child) = RealProbeOps.spawn(&mut command) else {
            panic!("spawn exit {code}");
        };
        let leg = LegProcess::Owned(child);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !leg.wait(Duration::from_millis(100)).unwrap() {
            assert!(Instant::now() < deadline, "exit {code} did not end within 30 s");
        }
        leg
    }

    /// An owned leg keeps how it ended: the exit wait reads the code and
    /// every later ask gives the same answer.
    #[test]
    fn an_owned_leg_keeps_its_exit_status() {
        for code in [71, 3] {
            let leg = ended(code);
            assert_eq!(leg.reap(), Some(ExitStatus::Code(code)), "exit {code}");
            assert_eq!(leg.reap(), Some(ExitStatus::Code(code)), "exit {code}: a second ask");
        }
    }
}
