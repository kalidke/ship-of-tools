//! The processes that hold a child's display (Linux): every process of this account whose environment names the
//! display's cookie file as its `XAUTHORITY`, in the child's process group or out of it (Pluto's notebook workers, a
//! process user code starts detached). A display's end ends them: once its server is gone any account can bind the
//! display's name, and a holder that opened the display then would send its pixels to that account's server.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// The most walks of the process table one end makes. A holder that forks between a walk and its kill leaves a child
/// only a later walk sees, and a killed holder forks no more, so the walks stop at the first that finds no new holder.
const WALKS: usize = 16;

/// Ends every process of this account whose environment holds `XAUTHORITY=<cookie>`. Each match is opened as a pidfd,
/// its environment is read again while that pidfd shows it alive (so the read was of the process the pidfd names), and
/// it is sent SIGKILL through the pidfd, so a pid the OS has given to another process is never signalled. A process
/// with SIGKILL pending runs no more code, so nothing waits for its exit. Returns how many it signalled.
pub(super) fn end(cookie: &Path) -> usize {
    let wanted = [b"XAUTHORITY=".as_slice(), cookie.as_os_str().as_bytes()].concat();
    // SAFETY: geteuid has no preconditions.
    let account = unsafe { libc::geteuid() };
    let mut signalled: Vec<(u32, OwnedFd)> = Vec::new();
    for _ in 0..WALKS {
        let mut new = 0;
        for pid in processes() {
            // A holder signalled by an earlier walk that has not exited: the pid still names it.
            if signalled.iter().any(|(p, fd)| *p == pid && !exited(fd)) || !holds(pid, account, &wanted) {
                continue;
            }
            let Some(fd) = open(pid) else { continue };
            if !holds(pid, account, &wanted) || exited(&fd) {
                continue;
            }
            match kill(&fd) {
                Ok(()) => {
                    signalled.push((pid, fd));
                    new += 1;
                }
                Err(e) if e.raw_os_error() == Some(libc::ESRCH) => {}
                Err(e) => {
                    tracing::warn!(pid, error = %e, "a process that holds an ended display's cookie was not signalled")
                }
            }
        }
        if new == 0 {
            if !signalled.is_empty() {
                tracing::info!(ended = signalled.len(), "the processes that held an ended display's cookie are ended");
            }
            return signalled.len();
        }
    }
    tracing::warn!(walks = WALKS, "processes that hold an ended display's cookie were still starting at the last walk");
    signalled.len()
}

/// Every process id the process table lists now.
fn processes() -> Vec<u32> {
    match std::fs::read_dir("/proc") {
        Ok(dir) => dir.flatten().filter_map(|entry| entry.file_name().to_str()?.parse().ok()).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "the process table is unreadable, so no holder of an ended display is ended");
            Vec::new()
        }
    }
}

/// Whether `pid` is a process of `account` whose environment holds `wanted` exactly. A process shows the environment it
/// started with; a zombie shows none.
fn holds(pid: u32, account: u32, wanted: &[u8]) -> bool {
    let dir = format!("/proc/{pid}");
    std::fs::metadata(&dir).is_ok_and(|meta| meta.uid() == account)
        && std::fs::read(format!("{dir}/environ")).is_ok_and(|env| env.split(|b| *b == 0).any(|v| v == wanted))
}

/// The process `pid` names now, as a pidfd; none once it is gone.
fn open(pid: u32) -> Option<OwnedFd> {
    // SAFETY: pidfd_open takes a pid and flags and returns a descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            tracing::warn!(pid, error = %e, "a process that holds an ended display's cookie could not be opened");
        }
        return None;
    }
    // SAFETY: a new descriptor that nothing else owns.
    Some(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

/// Whether the process a pidfd names has exited: the pidfd is readable from its exit on, reaped or not.
fn exited(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd, and no wait.
    unsafe { libc::poll(&mut poll, 1, 0) > 0 }
}

/// SIGKILL through the pidfd: it reaches the process the pidfd names, never a later holder of its pid.
fn kill(fd: &OwnedFd) -> std::io::Result<()> {
    // SAFETY: pidfd_send_signal on a descriptor the caller owns, with no siginfo and no flags.
    let sent = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if sent == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
