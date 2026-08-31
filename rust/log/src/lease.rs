//! ADR 0041 step 6 U2: the parent-death lease a spawned capsule checks
//! as its FIRST act after acquiring the writer fence (Lifecycle
//! "Discovery, and the two windows a spawn passes through" —
//! `voyage::VoyageStore::open_prepared`'s own `lease_broken` parameter
//! already implements the CHECK's placement; this module is where the
//! lease itself comes from). "The child's first act after acquiring the
//! fence is to check the parent-death lease it was spawned with (an
//! inherited, synchronizable handle to its supervisor); if the lease is
//! broken it releases the fence and exits without binding."
//!
//! A named, kernel-brokered MUTEX — never a raw inherited process
//! HANDLE. The supervisor CREATES the mutex owning it
//! (`bInitialOwner = TRUE`) and never releases or closes it for the rest
//! of its own process life; the CHILD OPENS it BY NAME and polls
//! `WaitForSingleObject(handle, 0)`. While the supervisor lives, that
//! call returns `WAIT_TIMEOUT` (still owned, so a zero-length wait times
//! out) — "still held" IS "still alive". If the supervisor's process
//! ends by ANY means — clean exit, crash, or a hard kill — while holding
//! an owned mutex, Windows itself marks it ABANDONED, and every waiter's
//! next wait returns `WAIT_ABANDONED_0` immediately: the kernel's own
//! death signal, requiring no inherited-HANDLE plumbing through
//! `CreateProcess`'s attribute list and no lifetime-counter polling of
//! process liveness (the discipline this crate's own review history
//! already pins: "never observe kernel-lock ownership with a lifetime
//! counter"). The mutex's OWNING THREAD does not matter — abandonment on
//! process exit is a property of the PROCESS, not a specific thread —
//! so the supervisor need only keep [`Lease`] alive for its own life and
//! never call `ReleaseMutex`.
//!
//! The name is passed to the child as a plain string CLI argument, baked
//! into its command line BEFORE `CreateProcess` — no attribute-list
//! handle inheritance, no chicken-and-egg with a child pid that does not
//! exist yet when the name is chosen. Any outcome OTHER than a clean
//! `WAIT_TIMEOUT` (abandoned, unexpectedly signaled, a wait failure, or
//! the name simply not openable at all) is treated as broken — fail
//! closed on ambiguity, matching the ADR's own "if the lease is broken"
//! framing rather than "if we can positively prove it is intact".

#![cfg(windows)]

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE, WAIT_ABANDONED_0, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{CreateMutexW, MUTEX_MODIFY_STATE, OpenMutexW, ReleaseMutex, WaitForSingleObject};

/// The standard `SYNCHRONIZE` access right (WinNT.h) — the one
/// `OpenMutexW` needs here. Inlined rather than pulled from
/// `windows_sys::Win32::System::SystemServices`: that module needs its
/// own `Win32_System_SystemServices` Cargo feature enabled for this one
/// constant, where every other access right this crate already uses
/// (`PROCESS_SYNCHRONIZE`, `PROCESS_TERMINATE`, ...) comes from a feature
/// already on. The value is architecturally stable (a documented,
/// never-changing standard access-rights bit).
const SYNCHRONIZE: u32 = 0x0010_0000;

/// `Local\sot-lease-<h>-<pid>` — `h` is the caller's own stable hash of
/// the canonicalized state-dir path, the SAME one the supervisor lane's
/// own pipe name uses (ADR 0041: "the same thing that scopes the
/// pointer and the fence"); `pid` is the CREATING supervisor's own
/// process id, which is what makes the name unique across SUCCESSIVE
/// instances of the same drawer's supervisor without needing a random
/// nonce (a dead supervisor's own mutex object vanishes once its last
/// handle — its own — closes at process exit, so a same-pid collision
/// can never happen and a different-pid successor never collides with
/// a still-live predecessor either, since that predecessor would still
/// hold the fence).
pub fn lease_name(h: &str, supervisor_pid: u32) -> String {
    format!(r"Local\sot-lease-{h}-{supervisor_pid:x}")
}

/// The supervisor's own held lease: created once, at startup, OWNED for
/// the rest of this process's life, and never explicitly released.
/// Dropping this — equivalently, this process exiting by any means —
/// is exactly what marks the mutex abandoned for every child that opened
/// it: the death signal IS the drop/exit, not a separate act this crate
/// must remember to perform.
#[derive(Debug)]
pub struct Lease {
    #[allow(dead_code)] // held for Drop; the owned kernel object is the entire point
    handle: OwnedHandle,
}

/// Create and take ownership of a FRESH named mutex for `name` — the
/// supervisor's own call, exactly once, at startup. `GetLastError`/
/// `std::io::Error::last_os_error()` is read IMMEDIATELY after the OS
/// call, before any other call (including the cleanup `CloseHandle`
/// below), per this crate's own discipline. A name collision
/// (`ERROR_ALREADY_EXISTS` — `CreateMutexW` still returns a live handle
/// to the EXISTING object in that case) is refused loudly rather than
/// silently adopted: this caller is not that object's owner, and the
/// caller's own uniqueness contract (state-dir hash + this process's own
/// pid) having collided at all means something is already wrong that
/// pretending to own a stranger's mutex would only hide.
pub fn create(name: &str) -> std::io::Result<Lease> {
    let wide = wide_null(name);
    let raw = unsafe { CreateMutexW(std::ptr::null(), 1, wide.as_ptr()) };
    let err = std::io::Error::last_os_error();
    if raw.is_null() {
        return Err(err);
    }
    if err.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
        unsafe {
            CloseHandle(raw);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("lease name {name:?} already exists — refusing to adopt a stranger's mutex"),
        ));
    }
    // SAFETY: `raw` is a just-created, uniquely-owned HANDLE from
    // `CreateMutexW` above (the AlreadyExists case, the only one where
    // Windows hands back a handle to an object THIS call did not
    // create, already returned above).
    let handle = unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) };
    Ok(Lease { handle })
}

/// Open `name` by handle, once — the CHILD's own call, immediately after
/// acquiring the writer fence. `Err` means the lease could not even be
/// opened (the supervisor already exited before this child got this far,
/// or a genuine OS failure) — [`LeaseCheck::is_broken`] cannot be asked
/// at all in that case, so the caller must treat an `Err` here identically
/// to an opened-but-broken lease (fail closed), never as "no lease was
/// ever passed" (a caller that was never GIVEN a lease name in the first
/// place is a different, `None`-shaped case this module has no opinion
/// on — see `voyage::VoyageStore::open_prepared`'s own `Option`).
///
/// Opened with `SYNCHRONIZE | MUTEX_MODIFY_STATE`, not `SYNCHRONIZE`
/// alone: [`LeaseCheck::is_broken`] must call `ReleaseMutex` on the rare
/// accidental-ownership outcomes (see its own table), and `ReleaseMutex`
/// needs `MUTEX_MODIFY_STATE` — a handle opened with only `SYNCHRONIZE`
/// can wait on the mutex but silently fails to release it, since that
/// call's own `Err` had nowhere to go (Codex review round 1, finding 9).
pub fn open(name: &str) -> std::io::Result<LeaseCheck> {
    let wide = wide_null(name);
    let raw = unsafe { OpenMutexW(SYNCHRONIZE | MUTEX_MODIFY_STATE, 0, wide.as_ptr()) };
    let err = std::io::Error::last_os_error();
    if raw.is_null() {
        return Err(err);
    }
    // SAFETY: `raw` is a just-opened, uniquely-owned HANDLE from
    // `OpenMutexW` above.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) };
    Ok(LeaseCheck { handle })
}

/// An opened lease, ready to be polled — deliberately a SEPARATE type
/// from [`Lease`], never the same one: [`Lease`] OWNS the mutex (created
/// it with `bInitialOwner`), and the owning process waiting on its own
/// held mutex is a self-wait that proves nothing (it succeeds
/// immediately regardless of anything this lease is meant to observe);
/// this type only ever observes SOMEONE ELSE's ownership.
#[derive(Debug)]
pub struct LeaseCheck {
    handle: OwnedHandle,
}

impl LeaseCheck {
    /// `true` iff the lease is BROKEN — the supervisor that owns it has
    /// ended, by any means. The full outcome table for
    /// `WaitForSingleObject(handle, 0)`:
    ///
    /// | outcome            | meaning                                                                 | `is_broken` |
    /// |--------------------|--------------------------------------------------------------------------|-------------|
    /// | `WAIT_TIMEOUT`     | still owned by a live thread — a zero-length wait times out rather than completing | `false` |
    /// | `WAIT_ABANDONED_0` | the owning thread terminated without releasing — the death signal itself, AND this call itself now OWNS the mutex (Windows grants ownership to the thread that observes the abandonment). Released immediately, same as the row below, so a LATER independent checker (a different handle, thread, or process) still observes the true state instead of this checker's own now-held ownership masking it as `WAIT_TIMEOUT`. | `true` |
    /// | `WAIT_OBJECT_0`    | this call itself just ACQUIRED a mutex nobody currently holds — UNREACHABLE in the real cross-process topology (the supervisor holds it, unreleased, for its whole process life); only reachable via a caller checking from the SAME THREAD that also owns it, since Windows mutexes are recursively acquirable per-thread — a caller bug, never a supervisor-death signal. Released immediately (`ReleaseMutex`, never left extra-owned by the mere act of checking) and reported broken: an unexpected acquisition is not proof of a live supervisor. | `true` |
    /// | `WAIT_FAILED`      | the OS call itself failed                                                | `true` |
    ///
    /// Both ownership-granting outcomes (`WAIT_ABANDONED_0` and
    /// `WAIT_OBJECT_0`) release before returning, for the same reason:
    /// this is a CHECK, not a claim — leaving the mutex held by the
    /// checker's own thread would corrupt every subsequent check on this
    /// same lease, including the same checker's own next call. Release
    /// requires `MUTEX_MODIFY_STATE`, which [`open`] now requests
    /// alongside `SYNCHRONIZE` (Codex review round 1, finding 9: a
    /// `SYNCHRONIZE`-only handle makes `ReleaseMutex` silently fail).
    ///
    /// **Cross-context contract**: correct only when called from a thread
    /// that never itself acquires this same mutex — guaranteed by
    /// construction in production (the checker is a SEPARATE PROCESS from
    /// the creator). A same-thread check is a test artifact, never a real
    /// topology, and lands on the `WAIT_OBJECT_0` row above.
    pub fn is_broken(&self) -> bool {
        let raw = self.handle.as_raw_handle() as HANDLE;
        // SAFETY: `raw` is this struct's own live, owned handle.
        match unsafe { WaitForSingleObject(raw, 0) } {
            WAIT_TIMEOUT => false,
            WAIT_OBJECT_0 | WAIT_ABANDONED_0 => {
                // Never leave the mutex extra-owned by this checker's own
                // accidental/abandonment-granted acquisition (see the
                // table above) — release, then report broken.
                unsafe {
                    ReleaseMutex(raw);
                }
                true
            }
            _ => true, // WAIT_FAILED, or anything else
        }
    }
}

fn wide_null(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_name(tag: &str) -> String {
        // A real process's own pid plus a per-test tag: unique enough for
        // this test binary's own run without needing a full lease_name
        // round trip through a fake state-dir hash.
        format!(r"Local\sot-lease-test-{tag}-{:x}", std::process::id())
    }

    #[test]
    fn lease_name_is_stable_and_scoped_by_hash_and_pid() {
        assert_eq!(lease_name("abc123", 0x2a), r"Local\sot-lease-abc123-2a");
    }

    /// Windows mutex OWNERSHIP is per-THREAD, not per-handle or per-process
    /// (a thread that already owns a mutex re-acquires it recursively on a
    /// further wait, returning `WAIT_OBJECT_0` rather than `WAIT_TIMEOUT`
    /// -- see `LeaseCheck::is_broken`'s own table). `create` and `open`
    /// called from the SAME thread would therefore make every check see a
    /// recursive self-acquisition, never the real "still held by someone
    /// else" case production always is (the checker is a SEPARATE
    /// PROCESS). Every test below holds the lease on a DEDICATED thread
    /// and checks from the test's own (different) thread, so a genuine
    /// `WAIT_TIMEOUT` is what "not broken" actually observes.
    fn hold_on_a_thread(name: String) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let lease = create(&name).unwrap();
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv(); // park until told to finish
            drop(lease);
        });
        (ready_rx, release_tx, holder)
    }

    #[test]
    fn a_freshly_created_lease_is_not_broken_while_held() {
        let name = unique_name("held");
        let (ready_rx, release_tx, holder) = hold_on_a_thread(name.clone());
        ready_rx.recv().unwrap();
        let check = open(&name).unwrap();
        assert!(!check.is_broken());
        release_tx.send(()).unwrap();
        holder.join().unwrap();
    }

    #[test]
    fn dropping_the_owning_lease_marks_it_broken() {
        let name = unique_name("dropped");
        let (ready_rx, release_tx, holder) = hold_on_a_thread(name.clone());
        ready_rx.recv().unwrap();
        let check = open(&name).unwrap();
        assert!(!check.is_broken());
        release_tx.send(()).unwrap();
        // Abandonment is a property of the OWNING THREAD terminating, not
        // of the handle merely dropping -- join to be certain the thread
        // has actually exited before checking.
        holder.join().unwrap();
        assert!(check.is_broken());
    }

    #[test]
    fn a_second_create_for_the_same_name_is_refused() {
        let name = unique_name("dup");
        let _first = create(&name).unwrap();
        let err = create(&name).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn opening_a_name_nobody_created_fails() {
        let name = unique_name("never-created");
        assert!(open(&name).is_err());
    }

    #[test]
    fn multiple_independent_checks_all_observe_the_same_abandonment() {
        let name = unique_name("multi");
        let (ready_rx, release_tx, holder) = hold_on_a_thread(name.clone());
        ready_rx.recv().unwrap();
        let check_a = open(&name).unwrap();
        let check_b = open(&name).unwrap();
        assert!(!check_a.is_broken());
        assert!(!check_b.is_broken());
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert!(check_a.is_broken());
        assert!(check_b.is_broken());
    }
}
