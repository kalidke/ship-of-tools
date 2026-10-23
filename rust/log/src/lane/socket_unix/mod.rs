//! The Unix-domain-socket transport's client half: [`SocketClient`], the bounded non-blocking connect it comes from, and
//! its `Client`/`Endpoint` impls. The server half is the row socket's (`crate::row`).
//!
//! # Cancellation: `shutdown(2)`, no per-op cancel primitive
//!
//! POSIX has no way to target one blocked call from another thread. The primitive that generalizes is `shutdown(2)` on
//! the connection's fd: it ends a read waiting in `poll(2)` (the read then returns `0`, ordinary EOF) and a blocked
//! `write` (a partial count or `EPIPE`) BOTH AT ONCE, from any thread, without knowing which direction (if either) is
//! mid-call. A read never blocks inside `recv(2)` itself ([`read_when_ready`]): on macOS a shutdown landing as a
//! blocking read enters the kernel can be missed.
//!
//! # Visibility
//!
//! Every type below is `pub`, not `pub(crate)`: `tests/socket_unix/` is a separate integration-test crate and can only
//! reach a library's `pub` items.

#![cfg(unix)]

use crate::lane::client::Client;
// `Endpoint`'s only implementor here (`SocketEndpoint`) exists on the two Unix targets that have a peer-identity
// mechanism this crate trusts; a plain, unconditional `use` would warn "unused import" on any OTHER Unix build.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::lane::client::Endpoint;
// The platform's own challenge module for THIS transport, chosen ONCE by an alias, as `client::PlatformEndpoint` does.
// Linux reads `SO_PEERCRED` plus a pidfd pin; macOS reads `getpeereid` for the account and one `LOCAL_PEERTOKEN` audit
// token carrying the peer's pid AND the kernel's own reuse generation together. No other Unix has one.
#[cfg(target_os = "macos")]
use sot_platform::identity::challenge_macos as challenge_os;
#[cfg(target_os = "linux")]
use sot_platform::identity::challenge_unix as challenge_os;
use crate::lane::transport::{TransportError, CONNECT_BOUND};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

mod client;
mod connect;

pub(crate) use client::connect_unix_socket_unchallenged;
pub use client::SocketClient;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use client::SocketEndpoint;

/// Set O_NONBLOCK with a checked read-modify-write fcntl pair.
pub(super) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    if let Some(injected) = sot_platform::host::fd::flag_call() {
        return Err(injected);
    }
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One read that never sleeps inside `recv(2)`: wait in `poll(2)` until the socket is readable, at EOF or in error, then
/// take what is there with `MSG_DONTWAIT`, waiting again on a spurious wake. A `shutdown(2)` from another thread cannot
/// be missed by `poll`, whose readiness check and wait registration hold the socket lock together. A blocking read can
/// miss it on macOS: XNU's `soreceive` checks `SS_CANTRCVMORE`, then drops the socket lock in `uipc_rcvd` before
/// sleeping and rechecks only for data, so a shutdown landing there wakes nobody and the read sleeps until the peer's
/// next event (`sorflush` also zeroes `SO_RCVTIMEO`). Every socket read that a `shutdown(2)` must end goes through here.
pub(super) fn read_when_ready(stream: &UnixStream, buf: &mut [u8]) -> io::Result<usize> {
    let fd = stream.as_raw_fd();
    loop {
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for a socket `stream` keeps open.
        if unsafe { libc::poll(&mut ready, 1, -1) } < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        // SAFETY: `buf` is valid for `buf.len()` writable bytes for the whole call.
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::WouldBlock {
            return Err(e);
        }
    }
}
