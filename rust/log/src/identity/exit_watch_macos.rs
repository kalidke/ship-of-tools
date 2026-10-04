//! The macOS process-exit death watch: one `kqueue` per handle holding an `EVFILT_PROC`/`NOTE_EXIT` knote.
//! Shared by the challenged process (`challenge_macos`) and `probe_macos`'s not-yet-challenged child.

#![cfg(target_os = "macos")]

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

// ---------------------------------------------------------------------
// The death watch: one `kqueue` fd per handle, holding one
// `EVFILT_PROC`/`NOTE_EXIT` knote. `pub(crate)` for the same reason
// `challenge_unix::pidfd_open`/`poll_pidfd_readable` are (ADR 0043
// decision 21): `supervisor/probe/macos.rs`'s freshly spawned, not-yet-challenged
// child reuses THESE, rather than encoding the same two calls twice.
// ---------------------------------------------------------------------

/// Register a `NOTE_EXIT` knote for `pid` on a fresh `kqueue`, and
/// return the kqueue fd that now IS this process's death signal.
/// `Ok(None)` is `ESRCH`.
///
/// The knote attaches to the live `proc` the number currently names, so
/// what comes back identifies the INSTANCE -- which is the whole reason
/// this can stand in for a pidfd. Fails closed: `proc_find` does not
/// return zombies, so an already-exited pid is `ESRCH` rather than a
/// registration that will never fire, and a never-existed pid is the
/// same. What `ESRCH` MEANS depends on who owns the pid, and this
/// function deliberately does not decide -- it hands the caller an
/// `Option` and each call site states its own reading: for a peer we
/// did not spawn it is "unprovable" ([`challenge()`] returns
/// `Undetermined`); for `probe_macos`'s own unreaped child the zombie
/// pins the number, so it provably means "already exited" and that
/// handle is built already latched. Same errno, two correct answers,
/// neither flattened into this mechanism.
///
/// `EV_RECEIPT` with a one-entry eventlist is what makes the attach
/// result deterministic: the kernel always writes back one `EV_ERROR`
/// event carrying the errno in `data` (zero on success), instead of the
/// caller having to distinguish "kevent returned -1" from "the change
/// was rejected". No `EV_ONESHOT` and no `EV_CLEAR`: neither names an
/// invariant here, and the latch each owner carries already owns the
/// once-only semantics.
pub(crate) fn watch_exit(pid: u32) -> io::Result<Option<OwnedFd>> {
    // SAFETY: `kqueue()` takes no arguments and returns a fresh fd or -1.
    let raw = unsafe { libc::kqueue() };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a non-negative return from `kqueue(2)` is a freshly
    // created, valid, uniquely-owned fd. Note that a kqueue fd is not
    // inherited across `fork(2)` AT ALL, so unlike the lease pipe this
    // needs no `CLOEXEC` dance and can never leak into a leg.
    let kq = unsafe { OwnedFd::from_raw_fd(raw) };

    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let mut out: libc::kevent = unsafe { std::mem::zeroed() };
    let ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `kq` is live for the whole call; `change` and `out` are
    // one real `kevent` each and the counts say so; `ts` is a real
    // `timespec` (never NULL -- see `kevent_timeout`).
    let rc = unsafe { libc::kevent(kq.as_raw_fd(), &change, 1, &mut out, 1, &ts) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if rc == 0 {
        // `EV_RECEIPT` guarantees exactly one receipt event per change,
        // so this cannot happen; refuse rather than return a kqueue
        // whose knote was never confirmed.
        return Err(io::Error::other("kevent(EV_RECEIPT) returned no receipt"));
    }
    if (out.flags & libc::EV_ERROR) != 0 {
        return match out.data as libc::c_int {
            0 => Ok(Some(kq)), // `EV_RECEIPT`'s own success receipt
            libc::ESRCH => Ok(None),
            e => Err(io::Error::from_raw_os_error(e)),
        };
    }
    Ok(Some(kq))
}

/// Drain one `NOTE_EXIT` from a [`watch_exit`] kqueue: `Ok(true)` iff
/// the watched instance has exited, bounded by `timeout`, never
/// infinite. Raw -- no latch -- so the OWNER supplies the stickiness
/// (`NOTE_EXIT` is delivered once and then detached; a second call after
/// a successful one would wait out the whole timeout and answer
/// `false`, the exact inversion of the truth).
///
/// `EINTR` returns `Err`, matching `challenge_unix::poll_pidfd_readable`
/// rather than silently improving one platform. An `EV_ERROR` event
/// with a real errno is also `Err` and NOT an exit: this kqueue carries
/// exactly one knote, and reporting "the knote broke" as "the process
/// died" would retire a live leg. The ONE errno that is not "the knote
/// broke" is `ESRCH` -- the kernel saying the knote's own `proc` is not
/// there -- which is the very fact `NOTE_EXIT` reports, and it reads the
/// same for both owners: an ATTACHED watch (the only kind reaching this
/// function) names an instance, so "no such process" is that instance
/// gone. Unlike the ESRCH at attach time ([`watch_exit`]), it carries no
/// per-owner policy.
pub(crate) fn drain_exit(kq: RawFd, timeout: Duration) -> io::Result<bool> {
    let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
    let ts = kevent_timeout(timeout);
    // SAFETY: `kq` is a live kqueue owned by the caller for the whole
    // call; the eventlist is one real `kevent` and the count says so;
    // `ts` is a real `timespec`, never NULL.
    let rc = unsafe { libc::kevent(kq, std::ptr::null(), 0, &mut ev, 1, &ts) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if rc == 0 {
        return Ok(false);
    }
    // `data` is an errno ONLY on an `EV_ERROR` event -- on the real
    // `NOTE_EXIT` it carries the exit STATUS, which is why the flag is
    // checked first.
    if (ev.flags & libc::EV_ERROR) != 0 && ev.data != 0 {
        if ev.data as libc::c_int == libc::ESRCH {
            return Ok(true);
        }
        return Err(io::Error::from_raw_os_error(ev.data as i32));
    }
    Ok(ev.fflags & libc::NOTE_EXIT != 0)
}

/// `Duration` -> the `timespec` `kevent` takes by POINTER. The one
/// conversion here whose mistake would type-check, pass review and hang
/// the authority: a NULL timeout means BLOCK FOREVER, and the
/// supervisor polls every leg with `Duration::ZERO` on its own main-loop
/// tick (`reap_retired_legs`, `retire_leg`, the `Ready` tick). So
/// `Duration::ZERO` becomes `timespec { 0, 0 }` and the pointer is
/// always real. The far end saturates for the same reason
/// `challenge_unix::poll_pidfd_readable` clamps its millisecond count to
/// `i32::MAX`.
fn kevent_timeout(timeout: Duration) -> libc::timespec {
    let (tv_sec, tv_nsec) = kevent_timeout_parts(timeout);
    libc::timespec { tv_sec, tv_nsec }
}

/// [`kevent_timeout`]'s arithmetic, split out as pure integers so the
/// zero and the saturation are unit-testable without a `timespec`.
fn kevent_timeout_parts(timeout: Duration) -> (i64, i64) {
    match i64::try_from(timeout.as_secs()) {
        Ok(secs) => (secs, i64::from(timeout.subsec_nanos())),
        // Saturated: the sub-second remainder of a duration this long
        // is noise, and a `tv_nsec` beside `i64::MAX` seconds would only
        // risk `EINVAL` on a call that means "effectively forever".
        Err(_) => (i64::MAX, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one mapping in this module that a wrong answer would hide:
    /// `kevent`'s timeout is a POINTER and NULL means "block forever",
    /// so the supervisor's own non-blocking tick MUST arrive as a real
    /// `timespec { 0, 0 }`. Pure arithmetic, so this is a proof by
    /// construction rather than an observation of a running kernel.
    #[test]
    fn zero_timeout_is_a_real_zero_timespec() {
        assert_eq!(kevent_timeout_parts(Duration::ZERO), (0, 0));
        let ts = kevent_timeout(Duration::ZERO);
        assert_eq!(ts.tv_sec, 0);
        assert_eq!(ts.tv_nsec, 0);
    }

    #[test]
    fn sub_second_and_whole_timeouts_survive_the_split() {
        assert_eq!(
            kevent_timeout_parts(Duration::from_millis(1500)),
            (1, 500_000_000)
        );
        assert_eq!(kevent_timeout_parts(Duration::from_nanos(7)), (0, 7));
    }

    /// The far end saturates rather than wrapping, mirroring
    /// `challenge_unix::poll_pidfd_readable`'s own clamp to `i32::MAX`.
    #[test]
    fn an_unrepresentable_timeout_saturates() {
        assert_eq!(kevent_timeout_parts(Duration::MAX), (i64::MAX, 0));
    }
}
