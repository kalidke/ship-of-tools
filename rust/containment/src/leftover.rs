//! Test observer (feature `test-support`, Unix): the way a test holds itself to seeing the end of a process it did not
//! start itself. The end is read from a retained identity (a pidfd on Linux), never from a pid number the OS may have
//! given to a stranger.

use std::time::Duration;

/// A pid is gone once a probe fails, polled for up to 3 s (where there is no pidfd to read).
#[cfg(not(target_os = "linux"))]
fn gone(pid: i32) -> bool {
    (0..150).any(|_| {
        std::thread::sleep(Duration::from_millis(20));
        // SAFETY: signal 0 only probes the pid.
        unsafe { libc::kill(pid, 0) != 0 }
    })
}

/// A process a test started that is a descendant of a child, so the test holds no handle to it. A leftover is
/// built from its pid, or from a file the stub wrote the pid to, read when the pid is first needed; on Linux
/// the process is opened then as a pidfd, and from then on its end is read from that identity and never from the
/// number, which the OS may give to a stranger once init has reaped the process. The test must see it gone
/// ([`gone`](Self::gone)), or say it is alive at the test's end by design ([`kept_alive`](Self::kept_alive)); a
/// leftover dropped with neither fails the test, so a test that forgot to check the process it started cannot pass.
/// A leftover only watches: the pid came from a file a process wrote or from a descendant, not from this test's own
/// spawn, so the test never signals it. What a failing run leaves alive ends with the fixture's own bound (each stub
/// ends itself) or with the container the suite runs in.
pub struct Leftover {
    pid: std::cell::Cell<Option<i32>>,
    file: Option<std::path::PathBuf>,
    state: std::cell::Cell<Seen>,
    #[cfg(target_os = "linux")]
    identity: std::cell::RefCell<Option<std::os::fd::OwnedFd>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seen {
    Unchecked,
    Gone,
    KeptAlive,
}

impl Leftover {
    pub fn of_pid(pid: i32) -> Self {
        let leftover = Self::empty(Some(pid), None);
        leftover.open_identity(pid);
        leftover
    }

    pub fn of_file(file: impl Into<std::path::PathBuf>) -> Self {
        Self::empty(None, Some(file.into()))
    }

    fn empty(pid: Option<i32>, file: Option<std::path::PathBuf>) -> Self {
        Leftover {
            pid: std::cell::Cell::new(pid),
            file,
            state: std::cell::Cell::new(Seen::Unchecked),
            #[cfg(target_os = "linux")]
            identity: std::cell::RefCell::new(None),
        }
    }

    /// Open the process as an identity while it is alive. A process already gone has none to open: it is seen gone.
    #[cfg(target_os = "linux")]
    fn open_identity(&self, pid: i32) {
        use std::os::fd::FromRawFd;
        // SAFETY: pidfd_open takes a pid and flags and returns a descriptor or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd >= 0 {
            // SAFETY: a fresh descriptor nothing else owns.
            *self.identity.borrow_mut() =
                Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) });
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn open_identity(&self, _pid: i32) {}

    pub fn pid(&self) -> Option<i32> {
        if let (None, Some(file)) = (self.pid.get(), &self.file) {
            let pid = std::fs::read_to_string(file)
                .ok()
                .and_then(|text| text.trim().parse().ok());
            self.pid.set(pid);
            if let Some(pid) = pid {
                self.open_identity(pid);
            }
        }
        self.pid.get()
    }

    /// Whether the process is gone, polled for up to 3 s. On Linux this reads the identity (a pidfd readable at
    /// the exit, reaped or not); a process whose pid was never readable, or that was gone before it could be
    /// opened, counts as gone only in the second case.
    pub fn gone(&self) -> bool {
        let gone = match self.pid() {
            None => false,
            #[cfg(target_os = "linux")]
            Some(_) => self.identity.borrow().as_ref().map_or(true, |fd| {
                use std::os::fd::AsRawFd;
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                loop {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    let mut pfd = libc::pollfd {
                        fd: fd.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: one valid pollfd; a pidfd is readable once its process has exited.
                    let rc =
                        unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) };
                    // A child's SIGCHLD interrupts the wait: it goes on for what is left of the bound.
                    if rc < 0
                        && std::io::Error::last_os_error().kind()
                            == std::io::ErrorKind::Interrupted
                    {
                        continue;
                    }
                    break rc > 0;
                }
            }),
            #[cfg(not(target_os = "linux"))]
            Some(pid) => gone(pid),
        };
        if gone {
            self.state.set(Seen::Gone);
        }
        gone
    }

    /// This process is alive at the test's end by design: its stub ends itself.
    pub fn kept_alive(&self) {
        self.state.set(Seen::KeptAlive);
    }
}

impl Drop for Leftover {
    fn drop(&mut self) {
        if self.state.get() == Seen::Gone {
            return;
        }
        let pid = self.pid();
        if self.state.get() == Seen::Unchecked && pid.is_some() && !std::thread::panicking() {
            panic!("a Leftover (pid {}) was dropped without its end observed: the test never checked that the process it started is gone", pid.unwrap_or_default());
        }
    }
}