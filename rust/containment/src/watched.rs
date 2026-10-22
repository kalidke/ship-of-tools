//! Test observer (feature `test-support`): a retained identity of a process (a pidfd or a kqueue event), read for the process's end, never through a pid number the OS may have reused.

use std::time::Duration;

#[cfg(target_os = "linux")]
pub struct Watched(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl Watched {
    pub fn open(pid: u32) -> Self {
        use std::os::fd::FromRawFd;
        // SAFETY: open a retained identity of the still-ready fixture; the returned descriptor is owned here.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        assert!(
            fd >= 0,
            "cannot retain the ready fixture identity: {}",
            std::io::Error::last_os_error()
        );
        Self(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
    }
    /// Whether the process has ended, waiting up to 3 s. A signal that interrupts the wait is no answer (in a full run
    /// any child's SIGCHLD can land on this thread): the wait goes on for what is left of the bound.
    pub fn dead(&self) -> bool {
        use std::os::fd::AsRawFd;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let mut fd = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll this object's retained pidfd, which cannot observe a reused PID.
            let rc = unsafe { libc::poll(&mut fd, 1, left.as_millis() as libc::c_int) };
            if rc < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            return rc > 0 && fd.revents & libc::POLLIN != 0;
        }
    }

    pub fn cleanup(&self) {
        use std::os::fd::AsRawFd;
        // SAFETY: signal only the retained process identity, never a reused number.
        assert_eq!(
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.0.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            },
            0
        );
    }
}

#[cfg(target_os = "macos")]
pub struct Watched(std::os::fd::OwnedFd);

#[cfg(target_os = "macos")]
impl Watched {
    pub fn open(pid: u32) -> Self {
        use std::os::fd::FromRawFd;
        // SAFETY: retain an exit notification while the owned fixture is still ready and alive.
        let fd = unsafe { libc::kqueue() };
        assert!(fd >= 0, "cannot create a fixture identity watcher");
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        let event = libc::kevent {
            ident: pid as _,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        assert_eq!(
            unsafe { libc::kevent(fd, &event, 1, std::ptr::null_mut(), 0, std::ptr::null()) },
            0
        );
        Self(owned)
    }

    /// As on Linux: up to 3 s, and a signal that interrupts the wait is no answer.
    pub fn dead(&self) -> bool {
        use std::os::fd::AsRawFd;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let bound = libc::timespec {
                tv_sec: left.as_secs() as libc::time_t,
                tv_nsec: left.subsec_nanos() as libc::c_long,
            };
            // SAFETY: receive the retained event, never probing or signaling a reusable PID.
            let rc = unsafe {
                libc::kevent(
                    self.0.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    &bound,
                )
            };
            if rc < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            return rc == 1;
        }
    }
}
