//! Descriptor birth and the child's sweep: the one place a socket, a pipe or a pty gets its close-on-exec flag, and the
//! pre-exec pass that marks the rest, so no child started from this process holds a descriptor by accident.
//!
//! Linux makes the descriptor close-on-exec in the call that creates it (`SOCK_CLOEXEC`, `pipe2`, `O_CLOEXEC`). macOS
//! cannot: it flags a descriptor by a second call, and a child forked by another thread between the two would keep it.
//! There the constructor takes [`BIRTH_LOCK`] around create-and-flag, and every fork site takes the same lock around
//! its fork ([`fork_guard`]), so no fork sees a descriptor between the two calls. A descriptor born outside these
//! constructors (std's, tokio's, mio's) is covered by [`inherit_none`] in the child.
//!
//! A constructor owns what it makes the moment it exists, so any failure closes it. `role` names a descriptor for the
//! tests' birth recorder (`lane::test_progress`).

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
#[cfg(target_os = "macos")]
use std::sync::MutexGuard;
#[cfg(any(test, target_os = "macos"))]
use std::sync::Mutex;

/// Held by a constructor across create-and-flag and by a fork site across its fork. Used on macOS only.
#[cfg(target_os = "macos")]
static BIRTH_LOCK: Mutex<()> = Mutex::new(());

/// What a fork site holds across its fork: the birth lock on macOS, nothing elsewhere.
pub struct ForkGuard {
    #[cfg(target_os = "macos")]
    _held: MutexGuard<'static, ()>,
}

/// Taken by every constructor around create-and-flag and by every site that forks a child from this process, around
/// the fork, so no fork sees a descriptor between its creation and its close-on-exec flag. Never held across a call
/// to these constructors.
pub fn fork_guard() -> ForkGuard {
    ForkGuard {
        #[cfg(target_os = "macos")]
        _held: BIRTH_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
    }
}

/// What a constructor takes: the fork guard, and under test a record that this thread is about to ask for it.
fn birth_guard() -> ForkGuard {
    #[cfg(test)]
    {
        let (arrived, changed) = &ARRIVED;
        arrived.lock().unwrap_or_else(|e| e.into_inner()).push(std::thread::current().id());
        changed.notify_all();
    }
    fork_guard()
}

/// Test only: the threads that have reached a constructor's lock acquisition, so a test releases a held lock only
/// once its constructor is at the lock.
#[cfg(test)]
static ARRIVED: (Mutex<Vec<std::thread::ThreadId>>, std::sync::Condvar) = (Mutex::new(Vec::new()), std::sync::Condvar::new());

/// A socket born close-on-exec.
pub fn socket_cloexec(
    role: &'static str,
    domain: libc::c_int,
    kind: libc::c_int,
) -> io::Result<OwnedFd> {
    let _born = birth_guard();
    #[cfg(target_os = "linux")]
    let kind = kind | libc::SOCK_CLOEXEC;
    // SAFETY: socket(2) has no pointer arguments.
    let raw = unsafe { libc::socket(domain, kind, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = own(role, raw);
    flagged(role, &fd)?;
    Ok(fd)
}

/// A pipe, read end then write end, born close-on-exec and, when `nonblocking`, O_NONBLOCK.
pub fn pipe_cloexec(roles: [&'static str; 2], nonblocking: bool) -> io::Result<(OwnedFd, OwnedFd)> {
    let _born = birth_guard();
    let mut fds: [RawFd; 2] = [-1, -1];
    #[cfg(target_os = "linux")]
    // SAFETY: pipe2 fills the two-element array on success.
    let rc = unsafe {
        let nonblock = if nonblocking { libc::O_NONBLOCK } else { 0 };
        libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | nonblock)
    };
    #[cfg(not(target_os = "linux"))]
    // SAFETY: pipe fills the two-element array on success.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let (read, write) = (own(roles[0], fds[0]), own(roles[1], fds[1]));
    for (role, fd) in [(roles[0], &read), (roles[1], &write)] {
        flagged(role, fd)?;
    }
    #[cfg(not(target_os = "linux"))]
    if nonblocking {
        for fd in [&read, &write] {
            nonblock(fd.as_raw_fd())?;
        }
    }
    Ok((read, write))
}

/// A pty, master then slave, both born close-on-exec, the terminal sized `size`.
pub fn openpty_cloexec(size: &libc::winsize) -> io::Result<(OwnedFd, OwnedFd)> {
    let _born = birth_guard();
    let (master, slave) = open_pty_pair(size)?;
    let (master, slave) = (own("master", master), own("slave", slave));
    for (role, fd) in [("master", &master), ("slave", &slave)] {
        flagged(role, fd)?;
    }
    Ok((master, slave))
}

/// Linux has no `openpty` that sets the flag at birth: the same steps glibc's takes, each asking for it.
#[cfg(target_os = "linux")]
fn open_pty_pair(size: &libc::winsize) -> io::Result<(RawFd, RawFd)> {
    // SAFETY: each call acts on the descriptors this function just made; `name` is a buffer the size passed to
    // ptsname_r, and the master is owned from the moment it exists, so every error closes what was made.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = OwnedFd::from_raw_fd(master);
        if libc::grantpt(master.as_raw_fd()) != 0 || libc::unlockpt(master.as_raw_fd()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut name = [0 as libc::c_char; 128];
        let rc = libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len());
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        let slave = libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        );
        if slave < 0 {
            return Err(io::Error::last_os_error());
        }
        let slave = OwnedFd::from_raw_fd(slave);
        if libc::ioctl(
            master.as_raw_fd(),
            libc::TIOCSWINSZ as _,
            size as *const libc::winsize,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        use std::os::fd::IntoRawFd;
        Ok((master.into_raw_fd(), slave.into_raw_fd()))
    }
}

#[cfg(not(target_os = "linux"))]
fn open_pty_pair(size: &libc::winsize) -> io::Result<(RawFd, RawFd)> {
    let (mut master, mut slave): (libc::c_int, libc::c_int) = (-1, -1);
    let mut size = *size;
    // SAFETY: openpty fills both descriptors on success; the name and termios arguments are null; `size` is a local
    // copy, as the non-Linux signature takes a mutable pointer.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, slave))
}

/// Take ownership of a descriptor a call just returned, and record its birth for the tests.
fn own(role: &'static str, raw: RawFd) -> OwnedFd {
    // SAFETY: `raw` was just returned by the creating call and nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    crate::lane::test_progress::birth(role, fd.as_raw_fd());
    fd
}

/// Flag `fd` close-on-exec where the platform did not at birth, and check on every platform that it is.
fn flagged(role: &'static str, fd: &OwnedFd) -> io::Result<()> {
    if let Some(injected) = crate::lane::test_progress::flag_call() {
        return Err(injected);
    }
    let raw = fd.as_raw_fd();
    #[cfg(not(target_os = "linux"))]
    // SAFETY: fcntl on the descriptor `fd` owns.
    if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl on the descriptor `fd` owns.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::FD_CLOEXEC == 0 {
        return Err(io::Error::other(format!("{role} is not close-on-exec")));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn nonblock(raw: RawFd) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor the caller owns.
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL);
        if flags < 0 || libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// In a child between fork and exec: marks every descriptor above stdio close-on-exec, so the child keeps none of this
/// process's past its exec. A descriptor is not always close-on-exec from its birth: on macOS interprocess, mio and std
/// set the flag with a second call after creating a socket or a pipe, and a child started by another thread between the
/// two kept it, the daemon's listener and its accepted connections included. The descriptors this crate makes itself
/// are born through the constructors above, which close that window; this sweep covers the ones made elsewhere. Marked, not closed: std reports a failed
/// exec through a close-on-exec pipe it holds open until then. Only `install_lease` can hand the child a descriptor, by
/// clearing that one's flag after this call. A daemon child's sweep runs inside `Signal::spawn*`, after any `pre_exec`
/// of the caller; the window's ssh children (topology's `ssh_bridge`) call this from their own `pre_exec`, and the
/// daemon's ssh children (`LinkGate::command`) run it twice, from that `pre_exec` and from the `Signal`'s: harmless, the
/// second marks nothing new. Either way the child keeps none.
pub fn inherit_none() -> std::io::Result<()> {
    // SAFETY: close_range, getdtablesize and fcntl act on this process's own descriptor table; a number not open is
    // EBADF and changes nothing.
    unsafe {
        #[cfg(target_os = "linux")]
        if libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        ) == 0
        {
            return Ok(());
        }
        for fd in 3..libc::getdtablesize() {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn is_cloexec(fd: &OwnedFd) -> bool {
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        flags >= 0 && flags & libc::FD_CLOEXEC != 0
    }

    fn size() -> libc::winsize {
        libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    /// The descriptors from 3 up an unprepared child (no `pre_exec`) holds past its exec.
    fn held_by_plain_child() -> Vec<u32> {
        let mut sh = Command::new("sh");
        sh.arg("-c")
            .arg("n=3; while [ $n -lt 256 ]; do if test -e /dev/fd/$n; then echo $n; fi; n=$((n+1)); done");
        let ran = {
            let _fork = fork_guard();
            crate::test_wait::run_within(&mut sh, crate::test_isolated::ISOLATION_TIMEOUT).ok()
        };
        ran.stdout.lines().filter_map(|n| n.parse().ok()).collect()
    }

    #[test]
    fn a_socket_is_born_close_on_exec_and_no_plain_child_holds_it() {
        let fd = socket_cloexec("socket", libc::AF_UNIX, libc::SOCK_STREAM).unwrap();
        assert!(is_cloexec(&fd));
        assert!(!held_by_plain_child().contains(&(fd.as_raw_fd() as u32)));
    }

    #[test]
    fn a_pipe_is_born_close_on_exec_and_no_plain_child_holds_it() {
        for nonblocking in [false, true] {
            let (read, write) = pipe_cloexec(["pipe.read", "pipe.write"], nonblocking).unwrap();
            assert!(is_cloexec(&read) && is_cloexec(&write));
            let nonblock = |fd: &OwnedFd| {
                let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
                flags & libc::O_NONBLOCK != 0
            };
            assert_eq!(nonblock(&read) && nonblock(&write), nonblocking);
            let held = held_by_plain_child();
            assert!(!held.contains(&(read.as_raw_fd() as u32)));
            assert!(!held.contains(&(write.as_raw_fd() as u32)));
        }
    }

    #[test]
    fn a_pty_is_born_close_on_exec_sized_and_no_plain_child_holds_it() {
        let (master, slave) = openpty_cloexec(&size()).unwrap();
        assert!(is_cloexec(&master) && is_cloexec(&slave));
        let mut got: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCGWINSZ, &mut got) },
            0
        );
        assert_eq!((got.ws_row, got.ws_col), (24, 80));
        let held = held_by_plain_child();
        assert!(!held.contains(&(master.as_raw_fd() as u32)));
        assert!(!held.contains(&(slave.as_raw_fd() as u32)));
    }

    /// A constructor on another thread makes nothing while a fork site holds the lock, on macOS where the flag
    /// follows the creation; on Linux nothing waits, as the descriptor is born flagged. The thread reports whether the
    /// lock had been released when it finished, so no wait on a clock is needed.
    #[test]
    fn a_constructor_waits_for_a_fork_in_progress_only_where_the_flag_follows_the_birth() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let released = std::sync::Arc::new(AtomicBool::new(false));
        let guard = fork_guard();
        let seen = released.clone();
        let thread = std::thread::spawn(move || {
            let fd = socket_cloexec("socket", libc::AF_UNIX, libc::SOCK_STREAM).unwrap();
            (fd, seen.load(Ordering::SeqCst))
        });
        let (_fd, released_first) = if cfg!(target_os = "macos") {
            let id = thread.thread().id();
            crate::test_wait::within(crate::test_isolated::ISOLATION_TIMEOUT, "the constructor reaching the lock", move || {
                let (arrived, changed) = &ARRIVED;
                let mut seen = arrived.lock().unwrap_or_else(|e| e.into_inner());
                while !seen.contains(&id) {
                    seen = changed.wait(seen).unwrap_or_else(|e| e.into_inner());
                }
            });
            released.store(true, Ordering::SeqCst);
            drop(guard);
            crate::test_wait::within(crate::test_isolated::ISOLATION_TIMEOUT, "the constructor", move || {
                thread.join().unwrap()
            })
        } else {
            let made = crate::test_wait::within(crate::test_isolated::ISOLATION_TIMEOUT, "the constructor", move || {
                thread.join().unwrap()
            });
            drop(guard);
            made
        };
        assert_eq!(released_first, cfg!(target_os = "macos"));
    }
}
