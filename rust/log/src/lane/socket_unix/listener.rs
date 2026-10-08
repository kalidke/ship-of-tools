//! The listener: private runtime-dir check, fd-anchored bind, and socket flag helpers.

use super::*;

/// Create/verify the socket's parent directory (ADR 0043 decision 3): a
/// missing directory is created EXCLUSIVELY at mode `0700` (plain,
/// non-`_all` `create` + `DirBuilderExt::mode` maps to one `mkdir(2)` —
/// atomic, no window for a racing attacker to land a symlink in); a
/// present one gets a by-PATH PRE-check via
/// [`crate::host::state_dir::is_private_dir`] (lstat-based: real directory,
/// owned by this uid, owner-only) — NOT the authoritative check (module
/// doc "Security"): [`open_verified_dir_fd`], called immediately
/// afterward, re-verifies the SAME properties against the actually-opened
/// fd, which is what every later filesystem step is anchored to. Mirrors
/// `rust/backend/src/paths.rs::secure_private_dir`'s own contract exactly
/// (that function lives in a crate `sot-log` cannot depend on, so this is
/// a small, deliberate, self-contained duplicate rather than a new
/// dependency edge).
pub(super) fn ensure_private_runtime_dir(dir: &Path) -> Result<(), TransportError> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .map_err(|e| TransportError::Io {
                op: "mkdir(runtime dir)",
                source: e,
            }),
        Err(e) => Err(TransportError::Io {
            op: "stat(runtime dir)",
            source: e,
        }),
        Ok(_) if crate::host::state_dir::is_private_dir(dir) => Ok(()),
        Ok(_) => Err(TransportError::Io {
            op: "verify runtime dir",
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is not a private, owner-only directory (not a symlink, owned by this \
                     uid, mode 0700)",
                    dir.display()
                ),
            ),
        }),
    }
}

/// Module doc "Security", the AUTHORITATIVE check: open `dir` with
/// `O_NOFOLLOW`+`O_DIRECTORY` (a symlink leaf is rejected outright by the
/// OS itself, `ELOOP`, before this function's own check ever runs) and
/// `fstat` the resulting fd — real directory, owned by this uid,
/// owner-only. `ensure_private_runtime_dir`'s own by-path check only
/// APPROXIMATES this (a TOCTOU window exists between any by-path stat and
/// a later by-path operation); this one is what every later `*at()` call
/// in [`create_and_bind_listener`] and [`SocketServer::disconnect_listener`]
/// is anchored to, so the fd is kept open for the whole server's life
/// rather than closed once this check passes.
pub(super) fn open_verified_dir_fd(dir: &Path) -> Result<OwnedFd, TransportError> {
    let c_dir = CString::new(dir.as_os_str().as_bytes()).map_err(|_| TransportError::Io {
        op: "open(runtime dir)",
        source: io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime dir path contains a NUL byte",
        ),
    })?;
    let raw = unsafe {
        libc::open(
            c_dir.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(TransportError::Io {
            op: "open(runtime dir)",
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: `raw` is a freshly opened, valid, not-otherwise-owned fd.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd.as_raw_fd(), &mut st) };
    if rc != 0 {
        return Err(TransportError::Io {
            op: "fstat(runtime dir fd)",
            source: io::Error::last_os_error(),
        });
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFDIR
        || st.st_uid != crate::host::state_dir::current_uid()
        || st.st_mode & 0o077 != 0
    {
        return Err(TransportError::Io {
            op: "verify runtime dir fd",
            source: io::Error::other(
                "the opened runtime dir fd is not a real, owner-only directory owned by this uid",
            ),
        });
    }
    Ok(fd)
}

/// ADR 0043 decision 2: the caller HOLDS the endpoint's lifetime lock, so
/// a pre-existing socket file named `file_name` inside `dir_fd` is stale
/// by construction — unlinked via `unlinkat`, never probed. Module doc
/// "Security": every filesystem step below is anchored to `dir_fd` — the
/// ALREADY-VERIFIED directory fd [`open_verified_dir_fd`] returned, kept
/// open for the server's whole life — via `unlinkat`/`fchmodat`/`fstatat`,
/// never a fresh by-path lookup that could land in a since-swapped
/// ancestor. On Linux, even `bind` itself goes through the anchored
/// `/proc/self/fd/<dir_fd>/<file_name>` path (a magic symlink that always
/// resolves against the FD's own identity, never re-walking `path`'s own
/// ancestors) for the identical reason; other Unix targets bind by the
/// ordinary `path` (a narrower, documented guarantee there — macOS/BSD
/// support is experimental, ADR 0043's own "Open for the maintainer").
/// `libc::socket`/`bind`/`fchmodat`+verify/`listen` in that exact order
/// (ADR 0043 decision 3): `UnixListener::bind` alone would `bind` AND
/// `listen` together, leaving a window where the socket exists (and,
/// once `listen`ed, is connectable) before this transport has verified
/// its own permissions — so the listener is built by hand from raw
/// `libc` calls instead, and wrapped in a `UnixListener` only once
/// `listen` has already run.
pub(super) fn create_and_bind_listener(
    dir_fd: RawFd,
    file_name: &CStr,
    path: &Path,
    max_connections: u32,
) -> Result<UnixListener, TransportError> {
    let rc = unsafe { libc::unlinkat(dir_fd, file_name.as_ptr(), 0) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::NotFound {
            return Err(TransportError::Io {
                op: "unlinkat(stale socket)",
                source: err,
            });
        }
    }

    // Linux creates this socket with SOCK_STREAM | SOCK_CLOEXEC. macOS uses SOCK_STREAM followed immediately by checked
    // fcntl before publication; that creation-to-flagging window is not atomic.
    #[cfg(target_os = "linux")]
    let socket_type = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let socket_type = libc::SOCK_STREAM;
    let raw = unsafe { libc::socket(libc::AF_UNIX, socket_type, 0) };
    if raw < 0 {
        return Err(TransportError::Io {
            op: "socket(AF_UNIX)",
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: `raw` is a freshly created, valid, not-otherwise-owned fd.
    // Wrapped immediately so every early return below closes it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    crate::lane::test_progress::birth("listener", fd.as_raw_fd());
    #[cfg(not(target_os = "linux"))]
    set_cloexec(fd.as_raw_fd()).map_err(|e| TransportError::Io {
        op: "fcntl(FD_CLOEXEC socket)",
        source: e,
    })?;

    #[cfg(target_os = "linux")]
    assert_domain_is_unix(fd.as_raw_fd());

    // Whichever bind mechanism is used below, every future CLIENT's own
    // `connect()` must still address this socket by the REAL path -- so
    // ITS length is what must fit `sun_path`, independent of the (usually
    // much shorter) `/proc/self/fd` bind trick's own length.
    // `socket_path()` already enforces this for the production call
    // sites; reasserted here since this function is reachable directly (a
    // future in-crate caller, or a test).
    if path.as_os_str().as_bytes().len() > max_sun_path_bytes() {
        return Err(TransportError::PathTooLong(path.to_path_buf()));
    }

    #[cfg(target_os = "linux")]
    let addr_bytes_owned = {
        let mut v = format!("/proc/self/fd/{dir_fd}/").into_bytes();
        v.extend_from_slice(file_name.to_bytes());
        v
    };
    #[cfg(not(target_os = "linux"))]
    let addr_bytes_owned = path.as_os_str().as_bytes().to_vec();
    let addr_bytes = &addr_bytes_owned[..];
    // Belt and braces: the mechanism-specific bytes actually handed to
    // `bind` get their OWN reassertion too (on Linux this is a much
    // shorter string than `path`'s own and will essentially never trip;
    // on other Unix it's the identical check as above).
    if addr_bytes.len() > max_sun_path_bytes() {
        return Err(TransportError::PathTooLong(path.to_path_buf()));
    }
    // SAFETY: a zeroed `sockaddr_un` is a valid value of that type
    // (all-zero bytes for every field, including a NUL-filled
    // `sun_path`).
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, &b) in addr.sun_path.iter_mut().zip(addr_bytes) {
        *dst = b as libc::c_char;
    }
    let addr_len = (std::mem::size_of::<libc::sa_family_t>() + addr_bytes.len() + 1)
        as libc::socklen_t; // +1: the NUL terminator `sockaddr_un` expects, already zeroed in.

    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            std::ptr::addr_of!(addr).cast(),
            addr_len,
        )
    };
    if rc != 0 {
        // ADR 0043 decision 2: a real error, never a retry -- the caller
        // holds the endpoint's lifetime lock, so nothing legitimate
        // should ever be racing this bind.
        return Err(TransportError::Io {
            op: "bind(AF_UNIX)",
            source: io::Error::last_os_error(),
        });
    }

    // fchmodat 0600, then VERIFY via fstatat, BEFORE `listen` (ADR 0043
    // decision 3): no connection can exist before `listen` runs, so this
    // closes the window completely rather than merely narrowing it. Both
    // anchored to `dir_fd`/`file_name` (module doc "Security"), never a
    // fresh by-path lookup -- `fchmod` on the SOCKET FD ITSELF was tried
    // first and observed (this module's own test suite, real Linux) to
    // be a silent no-op against a just-bound `AF_UNIX` socket, which is
    // why this goes through the `*at()` pair instead.
    //
    // `fchmodat` passes flags `0`, NOT `AT_SYMLINK_NOFOLLOW` (Codex review
    // round 2, finding 3) -- this is a COMPATIBILITY choice, not a claim
    // that the flag is universally unusable: whether Linux's `fchmodat`
    // honours `AT_SYMLINK_NOFOLLOW` at all varies by kernel/glibc version
    // (it happens to work on this host's 5.15 kernel / glibc 2.35, but
    // that is not a property this crate can assume of every Linux target
    // it ships to), so `0` is the one value guaranteed to work everywhere.
    // It is also SAFE here regardless of that variance: the entry named
    // `file_name` was JUST created by the `bind` call immediately above,
    // inside a directory this module already verified is owner-only 0700
    // (`open_verified_dir_fd`), so nothing outside this same uid (out of
    // scope, module doc "Security") could have replaced it with a symlink
    // in the instant since -- and the very next call, `fstatat` with
    // `AT_SYMLINK_NOFOLLOW` (that flag IS passed there, well-supported
    // everywhere, no such variance), verifies the result is a real 0600
    // socket special file owned by this uid BEFORE `listen` ever runs, so
    // a symlink slipped in by this `fchmodat` call would still be caught
    // here rather than silently accepted.
    let rc = unsafe { libc::fchmodat(dir_fd, file_name.as_ptr(), 0o600, 0) };
    if rc != 0 {
        return Err(TransportError::Io {
            op: "fchmodat(socket)",
            source: io::Error::last_os_error(),
        });
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(dir_fd, file_name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW)
    };
    if rc != 0 {
        return Err(TransportError::Io {
            op: "fstatat(socket)",
            source: io::Error::last_os_error(),
        });
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || st.st_uid != crate::host::state_dir::current_uid()
        || st.st_mode & 0o777 != 0o600
    {
        return Err(TransportError::Io {
            op: "verify socket permissions",
            source: io::Error::other(
                "socket file is not an owner-only (0600) socket special file after fchmodat",
            ),
        });
    }

    let rc = unsafe { libc::listen(fd.as_raw_fd(), max_connections as libc::c_int) };
    if rc != 0 {
        return Err(TransportError::Io {
            op: "listen(AF_UNIX)",
            source: io::Error::last_os_error(),
        });
    }

    // SAFETY: `fd` was just bound and listened on as an `AF_UNIX`/
    // `SOCK_STREAM` socket; `UnixListener` takes ownership of exactly
    // that fd.
    Ok(unsafe { UnixListener::from_raw_fd(fd.into_raw_fd()) })
}

/// Set the descriptor flag with checked fcntl. This is a post-creation operation, not atomic descriptor creation.
#[cfg(not(target_os = "linux"))]
pub(super) fn set_cloexec(fd: RawFd) -> io::Result<()> {
    if let Some(injected) = crate::lane::test_progress::flag_call() {
        return Err(injected);
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Set O_NONBLOCK with a checked read-modify-write fcntl pair.
pub(super) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    if let Some(injected) = crate::lane::test_progress::flag_call() {
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

/// ADR 0043 decision 3, "`AF_UNIX` is asserted (property 2)": a pure
/// internal sanity check, not a caller-facing error path — this can only
/// fail if `create_and_bind_listener` stops actually requesting
/// `AF_UNIX`, which is a bug in THIS module, never a legitimate runtime
/// condition. `getsockopt(SO_DOMAIN)` itself is only documented on Linux
/// (hence `#[cfg(target_os = "linux")]` at the one call site); a
/// `getsockopt` failure (an older kernel lacking `SO_DOMAIN`) is silently
/// skipped rather than treated as a hard error, since it proves nothing
/// either way.
#[cfg(target_os = "linux")]
fn assert_domain_is_unix(fd: RawFd) {
    let mut domain: libc::c_int = -1;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_DOMAIN,
            std::ptr::addr_of_mut!(domain).cast(),
            &mut len,
        )
    };
    if rc == 0 {
        assert_eq!(domain, libc::AF_UNIX, "socket() did not create an AF_UNIX socket");
    }
}
