//! Windows half of the same-connection challenge (ADR 0041 Lifecycle "The
//! challenge"): steps 1-3 (`GetNamedPipeServerProcessId`, `OpenProcess`,
//! token-user SID comparison), the retained process-handle wrapper a
//! proof returns ([`ChallengedProcess`]), and the raw-handle extension
//! trait ([`PipeChallengeable`]) a connection needs to supply for them.
//! Steps 4-5 (the wire half) are shared, platform-neutral logic in
//! `crate::challenge` — see that module's own doc; [`challenge()`] and
//! [`authenticate_server()`] below call into it rather than
//! reimplementing it. A `challenge_unix.rs` counterpart lands in
//! L1-unix's LU1c.

#![cfg(windows)]

use crate::challenge::{
    exchange_identity, ChallengeOutcome, ChallengeableConnection, SidAuthOutcome, SidAuthenticated,
    StatusFailure,
};
use crate::exchange::IdentityExchange;
use crate::fsutil;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{FILETIME, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, TerminateProcess, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

/// The Windows-shaped extension every [`ChallengeableConnection`] this
/// crate actually challenges must also supply: raw pipe-handle access, for
/// `GetNamedPipeServerProcessId` (step 1). Separate from
/// `ChallengeableConnection` itself (L1-unix LU1a) so that trait can stay
/// platform-neutral — a Unix connection's own steps 1-3 (LU1c) will use
/// peer-credential syscalls instead of a raw handle, and has no reason to
/// carry this method at all.
pub trait PipeChallengeable: ChallengeableConnection {
    /// This end's own HANDLE for the connected pipe instance —
    /// `GetNamedPipeServerProcessId` resolves the peer from either end of
    /// a named pipe, so the CLIENT's own handle is exactly what step 1
    /// needs.
    fn raw_handle(&self) -> HANDLE;
}

/// A retained handle to a process this crate has PROVEN is the server
/// behind one challenged connection: `(handle, pid, creation FILETIME)`,
/// opened with exactly the access the ADR's two later uses need —
/// `PROCESS_TERMINATE` (the invalid-mgmt fallback's kill authority),
/// `PROCESS_QUERY_LIMITED_INFORMATION` (`GetProcessTimes`, both at proof
/// time and again at [`reverify`](Self::reverify)), `PROCESS_SYNCHRONIZE`
/// ([`wait`](Self::wait), the death signal). Dropping this closes the
/// handle. ONLY the full five-step [`challenge()`] ever produces one —
/// see [`SidAuthenticated`] for the deliberately weaker, deliberately
/// handle-less steps-1-3-only counterpart.
pub struct ChallengedProcess {
    handle: OwnedHandle,
    pid: u32,
    created: u64,
}

impl std::fmt::Debug for ChallengedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChallengedProcess")
            .field("pid", &self.pid)
            .field("created", &self.created)
            .finish_non_exhaustive()
    }
}

impl ChallengedProcess {
    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The creation time this handle was PROVEN against, as the exact
    /// FILETIME bits (`(high << 32) | low`) — the same packing
    /// `capsule_win.rs`'s own `self_status` uses for the wire's
    /// `status_ok.created` field, so a caller can compare the two
    /// directly with no repacking.
    pub fn created(&self) -> u64 {
        self.created
    }

    /// Re-read this handle's identity via `GetProcessTimes` and compare
    /// it against what this handle was proven with — the ADR's
    /// "pre-terminate re-verification" before an invalid-mgmt hard stop.
    /// `Ok(true)` is the only outcome a correct caller ever sees: a HELD
    /// handle keeps the SAME kernel process object alive regardless of
    /// PID reuse elsewhere, so this is defense-in-depth, not a scenario
    /// this crate expects to trigger.
    pub fn reverify(&self) -> std::io::Result<bool> {
        Ok(creation_filetime_bits(self.raw())? == self.created)
    }

    /// The death signal a supervisor waits on rather than sampling
    /// process absence (ADR 0041: "any operation needing that proof
    /// holds or re-acquires a process handle and waits on it"). See
    /// [`wait_handle`]'s own doc for the bound.
    pub fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        wait_handle(self.raw(), timeout)
    }

    /// The KILL half of the probe's own KILL+WAIT row, and the
    /// invalid-mgmt fallback's hard stop. Raw call only; sequencing
    /// (terminate, then `wait`) is the caller's.
    pub fn terminate(&self) -> std::io::Result<()> {
        terminate_handle(self.raw())
    }

    /// `GetExitCodeProcess`, mirroring
    /// `conpty::PrimaryProcess::exit_code_after_confirmed_exit`'s own
    /// precondition and honesty bound (that method's own doc has the full
    /// reasoning): the caller must have already observed [`wait`](Self::wait)
    /// return `true` before calling this -- `STILL_ACTIVE` (259) is also a
    /// value a process can legitimately exit WITH, so this makes no attempt
    /// to disambiguate "still running" from "exited with 259" and returns
    /// whatever the OS reports, unconditionally. Lets a caller that has
    /// ADOPTED a process (proven via the full challenge, not spawned by this
    /// process) classify its eventual exit the SAME way a spawned child's
    /// `ExitStatus::code()` does, once death is confirmed.
    pub fn exit_code_after_confirmed_exit(&self) -> std::io::Result<u32> {
        let mut code: u32 = 0;
        if unsafe { GetExitCodeProcess(self.raw(), &mut code) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(code)
    }
}

/// `WaitForSingleObject`, bounded (never Win32 `INFINITE` —
/// `fsutil::duration_to_wait_ms`'s guard). Shared by every retained
/// Windows process handle in this crate: [`ChallengedProcess::wait`]
/// above, and `crate::probe_win::SpawnedChild::wait` — the pre-proof owned
/// child, which needs the identical bounded wait but is deliberately a
/// DIFFERENT type (nothing has proven ITS identity yet).
pub(crate) fn wait_handle(handle: HANDLE, timeout: Duration) -> std::io::Result<bool> {
    let ms = fsutil::duration_to_wait_ms(timeout);
    match unsafe { WaitForSingleObject(handle, ms) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(std::io::Error::last_os_error()),
        other => Err(std::io::Error::other(format!("unexpected wait result {other:#x}"))),
    }
}

/// `TerminateProcess`. Shared by [`ChallengedProcess::terminate`] and
/// `crate::probe_win::SpawnedChild::terminate`.
pub(crate) fn terminate_handle(handle: HANDLE) -> std::io::Result<()> {
    if unsafe { TerminateProcess(handle, 1) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// `GetProcessTimes` on an already-open handle, packed to the exact bits
/// the wire's `status_ok.created` carries. `pub(crate)`:
/// `crate::probe_win::SpawnedChild` reuses this to read its OWN identity
/// (Codex review round 1, finding 10) — the same packing, over a
/// DIFFERENT handle (the owned, not-yet-proven child rather than a
/// challenged server's).
pub(crate) fn creation_filetime_bits(handle: HANDLE) -> std::io::Result<u64> {
    // SAFETY: four stack-local FILETIME out-params, valid to write into
    // regardless of the call's outcome.
    unsafe {
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }
}

/// Steps 1-3 of the OS-side identity check, shared by [`challenge()`] and
/// [`authenticate_server()`]: read the server pid `P` via
/// `GetNamedPipeServerProcessId`, `OpenProcess` it, and compare its
/// token-user SID against this account's. Returns the open handle plus
/// `P` on a matching SID; `Foreign`/`Undetermined` are already the
/// caller's own terminal outcome.
fn authenticate_steps_1_to_3(
    conn: &dyn PipeChallengeable,
) -> ChallengeOutcome<(OwnedHandle, u32)> {
    // Step 1.
    let mut server_pid: u32 = 0;
    if unsafe { GetNamedPipeServerProcessId(conn.raw_handle(), &mut server_pid) } == 0 {
        return ChallengeOutcome::Undetermined;
    }

    // Step 2.
    let access = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    let raw = unsafe { OpenProcess(access, 0, server_pid) };
    if raw.is_null() {
        return ChallengeOutcome::Undetermined;
    }
    // SAFETY: `raw` is a just-opened, uniquely-owned HANDLE from
    // `OpenProcess` above.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) };

    // Step 3: nothing past this point trusts, decodes, or acts on
    // anything from the peer until the SID matches.
    let their_sid = match fsutil::sid_string_from_process(handle.as_raw_handle() as HANDLE) {
        Ok(s) => s,
        Err(_) => return ChallengeOutcome::Undetermined,
    };
    let my_sid = match fsutil::token_user_sid_string() {
        Ok(s) => s,
        Err(_) => return ChallengeOutcome::Undetermined,
    };
    if their_sid != my_sid {
        return ChallengeOutcome::Foreign;
    }

    ChallengeOutcome::Proven((handle, server_pid))
}

/// The five pinned steps (ADR 0041 Lifecycle "The challenge"), in order:
/// (1) read the server pid `P` via `GetNamedPipeServerProcessId`; (2)
/// `OpenProcess(P, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION |
/// PROCESS_SYNCHRONIZE)`; (3) compare that process's token-user SID
/// against this account's; (4) only then `exchange`'s request on the SAME
/// connection; (5) proven iff the SID matched AND reply-pid == `P` AND
/// reply creation time == `GetProcessTimes(handle)` on the exact FILETIME
/// bits — pid compared FIRST, creation time queried only if it matches
/// (ADR 0041 U0 round-1 finding 6: an already-proven wrong pid must never
/// become `Undetermined` merely because `GetProcessTimes` also failed).
/// Nothing in the reply is decoded for meaning or acted on before step 3
/// succeeds.
///
/// `reply_deadline` bounds ONLY steps 4-5 — steps 1-3 are local,
/// synchronous OS calls with no wait to bound. Picking the deadline VALUE
/// (the ADR's "2s, clamped to the episode's remaining wall time") is the
/// probe classifier's job, a later unit; this function only enforces
/// whatever it is given. `Proven` here means the FULL five-step proof —
/// see [`authenticate_server()`] for the deliberately separate,
/// deliberately weaker steps-1-3-only operation (U1a Codex round-1,
/// Blocker 1: this function no longer takes an `Option` and no longer
/// returns `Proven` from steps 1-3 alone).
pub fn challenge(
    conn: &dyn PipeChallengeable,
    exchange: &mut dyn IdentityExchange,
    reply_deadline: Instant,
) -> ChallengeOutcome<ChallengedProcess> {
    let (handle, server_pid) = match authenticate_steps_1_to_3(conn) {
        ChallengeOutcome::Proven(v) => v,
        ChallengeOutcome::Foreign => return ChallengeOutcome::Foreign,
        ChallengeOutcome::Undetermined => return ChallengeOutcome::Undetermined,
    };

    // Steps 4-5: the lane's own request/reply, now the shared,
    // platform-neutral wire half (L1-unix LU1a) — see
    // `crate::challenge::exchange_identity`'s own doc for the full
    // reasoning this used to carry inline here.
    let c: &dyn ChallengeableConnection = conn;
    let exchange_result = exchange_identity(c, exchange, reply_deadline);

    let (reply_pid, reply_created) = match exchange_result {
        // The watchdog won the race (timed out, completed too late, or a
        // deadline could not even be established) — never proven, never
        // foreign.
        None => return ChallengeOutcome::Undetermined,
        Some(Ok(v)) => v,
        Some(Err(StatusFailure::Foreign)) => return ChallengeOutcome::Foreign,
        Some(Err(StatusFailure::Undetermined)) => return ChallengeOutcome::Undetermined,
    };

    // Finding 6: compare the pid BEFORE querying creation time, so a
    // provably wrong pid is ALWAYS Foreign, never Undetermined because
    // GetProcessTimes happened to also fail.
    if reply_pid != server_pid {
        return ChallengeOutcome::Foreign;
    }
    let created_now = match creation_filetime_bits(handle.as_raw_handle() as HANDLE) {
        Ok(c) => c,
        Err(_) => return ChallengeOutcome::Undetermined,
    };
    if reply_created != created_now {
        return ChallengeOutcome::Foreign;
    }

    ChallengeOutcome::Proven(ChallengedProcess {
        handle,
        pid: server_pid,
        created: created_now,
    })
}

/// ADR 0041 Lifecycle "The challenge", steps 1-3 ONLY: identify the peer
/// process behind a live connection and authenticate its token-user SID
/// against this account's. No wire I/O of any kind — this is why
/// `pipe_win::connect_voyage_pipe` (the shared, lane-agnostic constructor)
/// can call it unconditionally, regardless of which lane the caller's
/// FIRST frame will bind the connection to next (mgmt's `status`, or the
/// attach lane's `hello`): sending a lane-specific request here would
/// itself consume the connection's once-only first-frame lane binding
/// before the caller ever sends its own.
///
/// This is a WEAKER proof than [`challenge()`]'s full five steps — see
/// [`SidAuthenticated`]'s own doc for exactly what it does and does not
/// establish, and `connect_voyage_pipe`'s doc for the ADR's own
/// under-specification of the attach lane's stronger-proof story. A lane
/// that both wants and can afford the full proof (mgmt; the probe
/// classifier) runs `challenge()` itself, on top of a connection this
/// function already authenticated at the OS level.
pub fn authenticate_server(conn: &dyn PipeChallengeable) -> SidAuthOutcome {
    match authenticate_steps_1_to_3(conn) {
        ChallengeOutcome::Foreign => SidAuthOutcome::Foreign,
        ChallengeOutcome::Undetermined => SidAuthOutcome::Undetermined,
        ChallengeOutcome::Proven((handle, pid)) => {
            // `created` is read directly off the handle -- a fact about
            // the process, never about a reply this function never waits
            // for. The handle itself is then dropped: `SidAuthenticated`
            // retains nothing (see its own doc).
            match creation_filetime_bits(handle.as_raw_handle() as HANDLE) {
                Ok(created) => SidAuthOutcome::Authenticated(SidAuthenticated { pid, created }),
                Err(_) => SidAuthOutcome::Undetermined,
            }
        }
    }
}
