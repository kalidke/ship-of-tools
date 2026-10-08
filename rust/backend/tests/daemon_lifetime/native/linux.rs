//! Linux: an identity is a pidfd opened while the process was alive and checked against its start time. The pidfd
//! becomes readable at the process's exit and stays so after the reap, so an exit is seen however it was reaped.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

/// Field 22 of `/proc/<pid>/stat`: the start time in clock ticks, the process's birth identity next to its pid.
pub fn start_ticks(pid: i32) -> io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // The command name may hold spaces and parentheses; the fields that follow it start after the last ')'.
    let after = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest)
        .ok_or_else(|| io::Error::other("a /proc stat line without ')'"))?;
    after
        .split_whitespace()
        .nth(19)
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| io::Error::other("a /proc stat line without a start time"))
}

pub struct Identity {
    pub pid: i32,
    pub created: u64,
    pub label: String,
    pidfd: OwnedFd,
}

impl Identity {
    /// Open `pid` as an identity. With `created` (the process's own report of its start time) the pidfd is kept only
    /// if the process it opened has that start time, so a number that already named another process is refused. A
    /// process the fixture itself started and has not reaped needs none: its number cannot have been reused.
    pub fn acquire(pid: i32, created: Option<u64>, label: &str) -> io::Result<Identity> {
        // SAFETY: pidfd_open takes a pid and flags and returns a descriptor or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor nothing else owns.
        let pidfd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        let found = start_ticks(pid)?;
        if let Some(created) = created {
            if found != created {
                return Err(io::Error::other(format!(
                    "pid {pid} is not the process that reported start {created} (it has {found})"
                )));
            }
        }
        Ok(Identity {
            pid,
            created: found,
            label: label.to_string(),
            pidfd,
        })
    }

    /// Whether the process has exited, waiting up to `bound` for it.
    pub fn exited(&self, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let mut pfd = libc::pollfd {
                fd: self.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd; the timeout is clamped to what poll accepts.
            let rc = unsafe {
                libc::poll(
                    &mut pfd,
                    1,
                    left.as_millis().min(i32::MAX as u128) as libc::c_int,
                )
            };
            if rc > 0 {
                return true;
            }
            if rc == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return false;
            }
        }
    }

    /// `sig` through the pidfd: it names the process it opened, never a later holder of the number.
    pub fn signal(&self, sig: i32) -> io::Result<()> {
        // SAFETY: pidfd_send_signal on a descriptor this value owns, with no siginfo.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                sig,
                0,
                0,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(err)
    }

    /// SIGKILL through the pidfd: it names the process it opened, never a later holder of the number.
    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: pidfd_send_signal on a descriptor this value owns, with no siginfo.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                0,
                0,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(err)
    }
}
