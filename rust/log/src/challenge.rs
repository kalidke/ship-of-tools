//! ADR 0041's same-connection challenge (Lifecycle "The challenge"):
//! authenticates the SERVER behind a live pipe connection before any reply
//! is trusted, decoded for meaning, or acted on. One procedure for every
//! client of BOTH pipe families (today: the voyage pipe; later: the
//! supervisor lane) — a pipe's DACL is DIRECTIONAL, governing who may
//! CONNECT to the object the honest server made, and saying nothing about
//! who MADE the object a client found, so identity must be proven ON the
//! connection itself, by every client, never assumed from the pipe's own
//! ACL.
//!
//! U0 SCOPE: this module is the five pinned steps and the retained
//! process-handle wrapper a proof returns. It does NOT decide what a
//! `Proven`/`Foreign`/`Undetermined` result MEANS for readiness, adoption,
//! or respawn — the probe classifier's transition table (ADR 0041 "The
//! probe", Stage A/B) is a later unit's decision list, not this one's.
//! Nothing here is wired into `pipe_win::connect_voyage_pipe` either —
//! that's active behavior for today's clients (U1a), not a library with
//! no behavior change (U0).
//!
//! # Round-1 review: the exchange is now a lane seam, not a lane
//!
//! Steps 1-3 (identify the peer process, authenticate its token-user SID)
//! are the SAME procedure for every pipe lane and stay centralized in
//! [`challenge()`]. Only steps 4-5 (what bytes to send, what bytes count
//! as "the identity") vary per lane — the voyage mgmt lane's `status`
//! request today, the supervisor lane's own `status_ok {voyage, leg?,
//! phase}` protocol later — so [`crate::exchange::IdentityExchange`] is
//! the one thing a lane provides, and this function is the one thing
//! every lane shares: a lane cannot skip or reorder the OS steps, because
//! `challenge()` is the only caller of `IdentityExchange::feed`, and it
//! calls it only AFTER the SID has already matched. The deadline race
//! itself ([`crate::deadline::run_with_deadline`]) and the exchange
//! trait/codec ([`crate::exchange`]) are both portable — genuinely
//! tested on every CI platform, not merely compile-checked on Windows —
//! leaving only the actual Win32 authentication calls in this,
//! necessarily Windows-only, module.

#![cfg(windows)]

use crate::deadline::run_with_deadline;
use crate::exchange::{ExchangeDecode, IdentityExchange};
use crate::fsutil;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{FILETIME, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, TerminateProcess, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

/// One connection this crate can challenge: raw pipe-handle access (for
/// `GetNamedPipeServerProcessId`) plus the blocking read/write/cancel
/// shape every pipe family already exposes — mirrors
/// `pipe_win::PipeClient`'s own public API as a trait, so this module
/// depends on neither pipe family by name (ADR 0041: "one procedure for
/// both pipe families"). `Sync`: the deadline watchdog below calls
/// `cancel()` from a second thread while the caller's thread blocks in
/// `read`/`write_all`.
pub trait ChallengeableConnection: Sync {
    /// This end's own HANDLE for the connected pipe instance —
    /// `GetNamedPipeServerProcessId` resolves the peer from either end of
    /// a named pipe, so the CLIENT's own handle is exactly what step 1
    /// needs.
    fn raw_handle(&self) -> HANDLE;
    fn write_all(&self, bytes: &[u8]) -> std::io::Result<()>;
    /// `Ok(0)` is ordered EOF — never a spurious zero-byte completion.
    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize>;
    /// Abort whatever is in flight, from another thread. One-shot: after
    /// this, the connection is not expected to serve a further request
    /// (matches `PipeClient::cancel`'s own latch-`Closing` contract) — a
    /// challenged connection is mgmt-lane-latched for its whole life
    /// anyway (ADR 0041 wire framing), so nothing legitimate reuses it
    /// past one `status` round trip.
    fn cancel(&self);
}

/// What the challenge concluded. Generic over the retained-process type
/// so `probe::ProbeOps` (an associated-type seam) can drive this same
/// three-way split with a cheap dummy type in tests, while the real
/// [`challenge()`] free function always instantiates
/// `ChallengeOutcome<ChallengedProcess>`.
#[derive(Debug)]
pub enum ChallengeOutcome<P> {
    /// SID matched, and the reply's pid/creation matched what
    /// `GetNamedPipeServerProcessId` + `GetProcessTimes` independently
    /// observed. Carries the retained process handle — the ADR's death
    /// signal and pre-terminate re-verification both need a LIVE handle,
    /// not a remembered pid a later `OpenProcess` could resolve to a
    /// recycled process.
    Proven(P),
    /// A well-formed WRONG answer: a SID mismatch, a wrong pid/creation,
    /// or anything `IdentityExchange::feed` classified `Foreign`. An
    /// unproven server — never retried as if it might still be
    /// legitimate.
    Foreign,
    /// Any OS-call failure (`GetNamedPipeServerProcessId`, `OpenProcess`,
    /// `OpenProcessToken`, `GetTokenInformation`, `GetProcessTimes`), EOF,
    /// a timeout, or a watchdog that could not even be established —
    /// anywhere in the five steps. Never classified as proven or foreign
    /// (ADR 0041: "a failure ... is PENDING, never READY and never
    /// ADOPTED").
    Undetermined,
}

/// A retained handle to a process this crate has PROVEN is the server
/// behind one challenged connection: `(handle, pid, creation FILETIME)`,
/// opened with exactly the access the ADR's two later uses need —
/// `PROCESS_TERMINATE` (the invalid-mgmt fallback's kill authority),
/// `PROCESS_QUERY_LIMITED_INFORMATION` (`GetProcessTimes`, both at proof
/// time and again at [`reverify`](Self::reverify)), `PROCESS_SYNCHRONIZE`
/// ([`wait`](Self::wait), the death signal). Dropping this closes the
/// handle.
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
}

/// `WaitForSingleObject`, bounded (never Win32 `INFINITE` —
/// `fsutil::duration_to_wait_ms`'s guard). Shared by every retained
/// Windows process handle in this crate: [`ChallengedProcess::wait`]
/// above, and `probe::SpawnedChild::wait` — the pre-proof owned child,
/// which needs the identical bounded wait but is deliberately a DIFFERENT
/// type (nothing has proven ITS identity yet).
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
/// `probe::SpawnedChild::terminate`.
pub(crate) fn terminate_handle(handle: HANDLE) -> std::io::Result<()> {
    if unsafe { TerminateProcess(handle, 1) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// `GetProcessTimes` on an already-open handle, packed to the exact bits
/// the wire's `status_ok.created` carries.
fn creation_filetime_bits(handle: HANDLE) -> std::io::Result<u64> {
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
/// whatever it is given.
pub fn challenge(
    conn: &dyn ChallengeableConnection,
    exchange: &mut dyn IdentityExchange,
    reply_deadline: Instant,
) -> ChallengeOutcome<ChallengedProcess> {
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

    // Steps 4-5: the lane's own request/reply, bounded by the shared
    // three-state watchdog (finding 4). `encode_request()` runs INSIDE
    // the deadline-bounded body (round-2 finding 2): an already-expired
    // deadline must never call the lane at all, and a blocking future
    // lane's own `encode_request()` must not be able to defeat the bound
    // by running before the watchdog even exists.
    let exchange_result = run_with_deadline(
        reply_deadline,
        || conn.cancel(),
        move || -> Result<(u32, u64), StatusFailure> {
            let request = exchange.encode_request();
            conn.write_all(&request).map_err(|_| StatusFailure::Undetermined)?;
            let mut buf = [0u8; 512];
            loop {
                let n = conn.read(&mut buf).map_err(|_| StatusFailure::Undetermined)?;
                if n == 0 {
                    return Err(StatusFailure::Undetermined); // ordered EOF mid-challenge
                }
                match exchange.feed(&buf[..n]) {
                    ExchangeDecode::Incomplete => continue,
                    ExchangeDecode::Identity { pid, created } => return Ok((pid, created)),
                    ExchangeDecode::Foreign => return Err(StatusFailure::Foreign),
                }
            }
        },
    );

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

enum StatusFailure {
    Foreign,
    Undetermined,
}
