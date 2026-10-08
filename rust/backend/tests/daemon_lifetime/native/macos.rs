//! macOS: an identity is a kqueue holding an `EVFILT_PROC`/`NOTE_EXIT` knote on the process, attached while it was alive
//! and checked against its start time. The knote names the process instance, not the number, so an exit is seen however
//! the process is reaped. A case signals only a pid its own code got back from its own spawn (`acquire_own`); every other
//! identity is watched. No task-control right is needed, since nothing here ends a process the case did not start.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The process's start time in microseconds (`proc_bsdinfo`), the birth identity next to its pid.
pub fn start_ticks(pid: i32) -> io::Result<u64> {
    // SAFETY: a zeroed proc_bsdinfo is a valid out-parameter for proc_pidinfo.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: one proc_bsdinfo of the stated size is written for a pid; a stranger's pid fails.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if written != size {
        return Err(io::Error::other(format!(
            "no start time for pid {pid} (proc_pidinfo wrote {written} of {size} bytes)"
        )));
    }
    Ok(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

pub struct Identity {
    pub pid: i32,
    pub created: u64,
    pub label: String,
    /// Whether this case's own code got the pid back from its own spawn. Only such an identity can be signalled.
    own: bool,
    exits: OwnedFd,
    /// `NOTE_EXIT` is delivered once and then detached: the first sight of the exit is kept.
    gone: AtomicBool,
}

impl Identity {
    /// Open `pid` as an identity to watch: the process is observed and never signalled. With `created` (the process's own
    /// report of its start time) the identity is kept only if the process it attached to has that start time.
    pub fn acquire(pid: i32, created: Option<u64>, label: &str) -> io::Result<Identity> {
        Self::open(pid, created, label, false)
    }

    /// Open the pid this case's own code got back from its own spawn and has not reaped: its number cannot have been
    /// reused, and the identity can be signalled.
    pub fn acquire_own(pid: i32, label: &str) -> io::Result<Identity> {
        Self::open(pid, None, label, true)
    }

    fn open(pid: i32, created: Option<u64>, label: &str, own: bool) -> io::Result<Identity> {
        let before = start_ticks(pid)?;
        let exits = watch_exit(pid)?;
        // The knote attached to the instance the number named at that moment: it is the one read before only if the
        // start time still reads the same.
        let found = start_ticks(pid)?;
        if found != before {
            return Err(io::Error::other(format!(
                "pid {pid} named another process while it was being attached to"
            )));
        }
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
            own,
            exits,
            gone: AtomicBool::new(false),
        })
    }

    /// Whether the process has exited, waiting up to `bound` for it.
    pub fn exited(&self, bound: Duration) -> bool {
        if self.gone.load(Ordering::SeqCst) {
            return true;
        }
        let deadline = Instant::now() + bound;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let timeout = libc::timespec {
                tv_sec: left.as_secs() as libc::time_t,
                tv_nsec: left.subsec_nanos() as libc::c_long,
            };
            // SAFETY: a zeroed kevent is a valid out-parameter.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            // SAFETY: a live kqueue this value owns, one real event for the answer, a real timeout.
            let rc = unsafe {
                libc::kevent(
                    self.exits.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    &timeout,
                )
            };
            if rc > 0 {
                // An error event on this one knote is the kernel saying the process is not there: also the exit.
                self.gone.store(true, Ordering::SeqCst);
                return true;
            }
            if rc == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return false;
            }
        }
    }

    /// Whether this case started the process (the only kind it may signal).
    pub fn is_own(&self) -> bool {
        self.own
    }

    /// `sig` to the process, for a process this case started and has not reaped (its number cannot name another process).
    /// Refused for a process this case did not start.
    pub fn signal(&self, sig: i32) -> io::Result<()> {
        if !self.own {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} (pid {}) was not started by this case: it is watched, never signalled",
                    self.label, self.pid
                ),
            ));
        }
        // SAFETY: a signal to the unreaped child this case spawned.
        if unsafe { libc::kill(self.pid, sig) } == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(err)
    }

    /// SIGKILL, for a process this case started.
    pub fn kill(&self) -> io::Result<()> {
        self.signal(libc::SIGKILL)
    }
}

/// A fresh kqueue holding one `NOTE_EXIT` knote on `pid`. `EV_RECEIPT` makes the attach result deterministic: the kernel
/// writes back one error event carrying the errno (zero on success).
fn watch_exit(pid: i32) -> io::Result<OwnedFd> {
    // SAFETY: `kqueue()` takes no arguments and returns a fresh descriptor or -1.
    let raw = unsafe { libc::kqueue() };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a non-negative return from `kqueue(2)` is a descriptor nothing else owns.
    let kq = unsafe { OwnedFd::from_raw_fd(raw) };
    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: a zeroed kevent is a valid out-parameter.
    let mut receipt: libc::kevent = unsafe { std::mem::zeroed() };
    let now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a live kqueue, one real change and one real receipt, a real timeout.
    let rc = unsafe { libc::kevent(kq.as_raw_fd(), &change, 1, &mut receipt, 1, &now) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if rc == 0 {
        return Err(io::Error::other("kevent(EV_RECEIPT) returned no receipt"));
    }
    if (receipt.flags & libc::EV_ERROR) != 0 && receipt.data != 0 {
        return Err(io::Error::from_raw_os_error(receipt.data as libc::c_int));
    }
    Ok(kq)
}
