use super::parent_lease_fd_broken;
use std::io;
use std::os::fd::RawFd;

fn make_pipe() -> (RawFd, RawFd) {
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe() failed: {:?}", io::Error::last_os_error());
    (fds[0], fds[1])
}

#[test]
fn alive_while_the_write_end_stays_open() {
    let (r, w) = make_pipe();
    assert!(
        !parent_lease_fd_broken(r),
        "a pipe with its write end still open must read as alive"
    );
    unsafe {
        libc::close(r);
        libc::close(w);
    }
}

#[test]
fn broken_once_the_write_end_closes() {
    let (r, w) = make_pipe();
    unsafe {
        libc::close(w);
    }
    assert!(
        parent_lease_fd_broken(r),
        "a pipe whose write end closed must read as broken"
    );
    unsafe {
        libc::close(r);
    }
}

#[test]
fn broken_for_a_missing_or_closed_fd() {
    assert!(
        parent_lease_fd_broken(-1),
        "a negative fd must read as broken"
    );
    // A definitely-closed fd number -- EBADF, not a real descriptor.
    // An fd number nothing in this test binary holds: descriptors are
    // allocated lowest-free-first, so the top of the table is never
    // reached. (Closing a fresh pipe and probing ITS number raced the
    // other test threads, which can reopen that number in between --
    // seen once as a flake in the lib suite.)
    let top = unsafe { libc::getdtablesize() } - 1;
    assert!(
        parent_lease_fd_broken(top),
        "an fd number that is not open must read as broken"
    );
}
