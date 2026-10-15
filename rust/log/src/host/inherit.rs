/// In a child between fork and exec: marks every descriptor above stdio close-on-exec, so the child keeps none of this
/// process's past its exec. A descriptor is not always close-on-exec from its birth: on macOS interprocess, mio and std
/// set the flag with a second call after creating a socket or a pipe, and a child started by another thread between the
/// two kept it, the daemon's listener and its accepted connections included. Marked, not closed: std reports a failed
/// exec through a close-on-exec pipe it holds open until then. Only `install_lease` can hand the child a descriptor, by
/// clearing that one's flag after this call. A daemon child's sweep runs inside `Signal::spawn*`, after any `pre_exec`
/// of the caller; the window's ssh children (topology's `ssh_bridge`) call this from their own `pre_exec`. Either way
/// the child keeps none.
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
