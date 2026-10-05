//! The bounded, non-blocking connect(2) attempt over a fresh socket.

use super::*;
use super::listener::{set_cloexec, set_nonblocking};

// ---------------------------------------------------------------------
// Connect (ADR 0043 decision 4, property 18): a bounded, non-blocking
// `connect(2)` retry loop over a FRESH `AF_UNIX` socket per attempt (a
// failed `connect(2)` leaves the socket itself unusable for a further
// attempt on most Unix implementations, the same reason
// `pipe_win::connect_named_pipe_unchallenged` re-issues `CreateFileW`
// rather than reusing a handle across retries).
// ---------------------------------------------------------------------

/// Outcome of one raw connect attempt.
pub(super) enum ConnectAttempt {
    /// `EAGAIN` (ADR 0043 decision 4: a full listen backlog) or `EINTR`
    /// (the attempt was interrupted before it could complete, saying
    /// nothing about whether a listener exists) — an ordinary race in a
    /// healthy multi-client server, retried within [`CONNECT_BOUND`].
    Retryable(io::Error),
    /// `ECONNREFUSED`/`ENOENT` (no listener at all — ADR 0043 decision
    /// 27) or anything else — surfaced immediately, never retried. An
    /// absent or refused endpoint is the caller's to poll at its own
    /// interval, not this function's to retry.
    Fatal(io::Error),
}

fn poll_writable(fd: RawFd, timeout: Duration) -> io::Result<()> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if rc == 0 {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "poll(POLLOUT) timed out waiting for a non-blocking connect to complete",
        ));
    }
    Ok(())
}

fn set_blocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One `socket`+`connect` attempt against `addr_bytes` (the real path,
/// never the server's own `/proc/self/fd` bind trick — a client always
/// dials the real name). A FRESH socket every call: a failed `connect(2)`
/// on `AF_UNIX` leaves the fd in an unspecified state for a further
/// attempt, so retrying reuses nothing. `deadline` is the SAME absolute
/// bound the caller's own outer retry loop shares (review round 2 fix):
/// the `EINPROGRESS` path's own `poll_writable` call polls only for
/// `deadline.saturating_duration_since(Instant::now())`, never the full
/// [`CONNECT_BOUND`] on every attempt — an interrupted poll near the
/// deadline must not be able to overrun it by another whole
/// `CONNECT_BOUND`.
pub(super) fn one_connect_attempt(addr_bytes: &[u8], deadline: Instant) -> Result<UnixStream, ConnectAttempt> {
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(ConnectAttempt::Fatal(io::Error::last_os_error()));
    }
    // SAFETY: `raw` is a freshly created, valid, not-otherwise-owned fd.
    // Wrapped immediately so every early return below closes it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    if let Err(e) = set_cloexec(fd.as_raw_fd()) {
        return Err(ConnectAttempt::Fatal(e));
    }
    if let Err(e) = set_nonblocking(fd.as_raw_fd()) {
        return Err(ConnectAttempt::Fatal(e));
    }

    // SAFETY: a zeroed `sockaddr_un` is a valid value of that type.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, &b) in addr.sun_path.iter_mut().zip(addr_bytes) {
        *dst = b as libc::c_char;
    }
    let addr_len =
        (std::mem::size_of::<libc::sa_family_t>() + addr_bytes.len() + 1) as libc::socklen_t;

    #[allow(clippy::disallowed_methods, reason = "the one raw connect(2) of the unchallenged connector")]
    let rc = unsafe {
        libc::connect(fd.as_raw_fd(), std::ptr::addr_of!(addr).cast(), addr_len)
    };
    if rc != 0 {
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code) if code == libc::ECONNREFUSED || code == libc::ENOENT => {
                // ADR 0043 decision 27: no listener at all (the socket
                // does not exist, or exists but nothing is `accept`ing)
                // is fatal on the FIRST attempt — the caller's own poll
                // at its own interval owns waiting for the endpoint to
                // exist, not this bounded retry loop.
                return Err(ConnectAttempt::Fatal(err));
            }
            Some(code) if code == libc::EAGAIN || code == libc::EINTR => {
                // `EAGAIN` (ADR 0043 decision 4: a full listen backlog)
                // and `EINTR` (the call was interrupted by a caught
                // signal before it could complete) both say nothing
                // about whether a listener exists — only that this
                // ATTEMPT didn't finish. Dropping `fd` here (about to go
                // out of scope) cleanly aborts whatever the kernel had
                // started; the outer loop's own bounded retry (a fresh
                // socket, same absolute deadline) is the correct
                // recovery.
                return Err(ConnectAttempt::Retryable(err));
            }
            Some(code) if code == libc::EINPROGRESS => {
                // Cannot occur for AF_UNIX in practice (there is no
                // three-way handshake to be genuinely pending on) — but
                // if it ever did, wait for writability then read
                // SO_ERROR, exactly like a portable non-blocking TCP
                // connect would.
                //
                // Review round 2 fix: poll only for whatever remains of
                // the SAME absolute `deadline` the outer retry loop
                // shares -- not a fresh `CONNECT_BOUND` every time, which
                // could let an interrupted poll near the deadline overrun
                // it by another whole `CONNECT_BOUND`. An already-expired
                // deadline is classified `Retryable` without ever calling
                // `poll(2)` at all, so the outer loop's own
                // `Instant::now() >= deadline` check converts it into the
                // standard bounded-retry-exhaustion error -- never a
                // `Fatal` surfaced merely because the clock ran out.
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(ConnectAttempt::Retryable(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "connect bound already exhausted before the pending connect could be polled",
                    )));
                }
                if let Err(e) = poll_writable(fd.as_raw_fd(), remaining) {
                    // An interrupted `poll(2)` (EINTR) is the SAME "this
                    // attempt didn't finish, try again" case as
                    // `connect`'s own EINTR above -- Retryable within the
                    // SAME outer deadline, never Fatal.
                    return if e.kind() == io::ErrorKind::Interrupted {
                        Err(ConnectAttempt::Retryable(e))
                    } else {
                        Err(ConnectAttempt::Fatal(e))
                    };
                }
                let mut so_err: libc::c_int = 0;
                let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                let rc2 = unsafe {
                    libc::getsockopt(
                        fd.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        std::ptr::addr_of_mut!(so_err).cast(),
                        &mut len,
                    )
                };
                if rc2 != 0 {
                    return Err(ConnectAttempt::Fatal(io::Error::last_os_error()));
                }
                if so_err != 0 {
                    let err = io::Error::from_raw_os_error(so_err);
                    return if so_err == libc::EAGAIN {
                        Err(ConnectAttempt::Retryable(err))
                    } else {
                        // ECONNREFUSED/ENOENT (no listener — decision 27)
                        // and anything else are fatal here too.
                        Err(ConnectAttempt::Fatal(err))
                    };
                }
                // Fall through: connected.
            }
            _ => return Err(ConnectAttempt::Fatal(err)),
        }
    }

    if let Err(e) = set_blocking(fd.as_raw_fd()) {
        return Err(ConnectAttempt::Fatal(e));
    }
    // SAFETY: `fd` was just connected as an `AF_UNIX`/`SOCK_STREAM`
    // socket; `UnixStream` takes ownership of exactly that fd.
    Ok(unsafe { UnixStream::from_raw_fd(fd.into_raw_fd()) })
}

#[cfg(target_os = "linux")]
pub(super) fn capture_connect_anchor_boot_ticks() -> u64 {
    // A failure here degrades the eventual pin to `Undetermined`, never a
    // false `Proven` — see `challenge_unix::pin_peer`'s own strict
    // less-than check, which a `0` timestamp can only ever fail (no
    // process has a negative start time), so this never needs to fail
    // the connect outright over it.
    crate::identity::challenge_unix::boot_ticks_now().unwrap_or(0)
}
#[cfg(not(target_os = "linux"))]
pub(super) fn capture_connect_anchor_boot_ticks() -> u64 {
    0
}
