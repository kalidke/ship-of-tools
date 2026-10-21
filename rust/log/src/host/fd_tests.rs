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
