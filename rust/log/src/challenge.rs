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

#![cfg(windows)]

use crate::fsutil;
use crate::wire::{self, DecodedFrame, MgmtReply, MgmtRequest};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// What the challenge concluded.
#[derive(Debug)]
pub enum ChallengeOutcome {
    /// SID matched, and the reply's pid/creation matched what
    /// `GetNamedPipeServerProcessId` + `GetProcessTimes` independently
    /// observed. Carries the retained process handle — the ADR's death
    /// signal and pre-terminate re-verification both need a LIVE handle,
    /// not a remembered pid a later `OpenProcess` could resolve to a
    /// recycled process.
    Proven(ChallengedProcess),
    /// A well-formed WRONG answer: a SID mismatch, a wrong-but-decodable
    /// frame, a well-formed `status_ok` whose pid/creation does not match,
    /// or undecodable bytes / a frame over the wire cap. An unproven
    /// server — never retried as if it might still be legitimate.
    Foreign,
    /// Any OS-call failure (`GetNamedPipeServerProcessId`, `OpenProcess`,
    /// `OpenProcessToken`, `GetTokenInformation`, `GetProcessTimes`), EOF,
    /// or a timeout — anywhere in the five steps. Never classified as
    /// proven or foreign (ADR 0041: "a failure ... is PENDING, never
    /// READY and never ADOPTED").
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

    /// `WaitForSingleObject`, bounded — the death signal a supervisor
    /// waits on rather than sampling process absence (ADR 0041: "any
    /// operation needing that proof holds or re-acquires a process
    /// handle and waits on it"). `Ok(true)`: the process signaled
    /// (exited) within `timeout`. `Ok(false)`: the timeout elapsed; still
    /// running.
    pub fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        match unsafe { WaitForSingleObject(self.raw(), ms) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(std::io::Error::last_os_error()),
            other => Err(std::io::Error::other(format!("unexpected wait result {other:#x}"))),
        }
    }

    /// `TerminateProcess` — the KILL half of the probe's own KILL+WAIT
    /// row, and the invalid-mgmt fallback's hard stop. Raw call only;
    /// sequencing (terminate, then `wait`) is the caller's.
    pub fn terminate(&self) -> std::io::Result<()> {
        if unsafe { TerminateProcess(self.raw(), 1) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
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
/// against this account's; (4) only then `status` on the SAME connection;
/// (5) proven iff the SID matched AND reply-pid == `P` AND reply creation
/// time == `GetProcessTimes(handle)` on the exact FILETIME bits. Nothing
/// in the reply is decoded for meaning or acted on before step 3
/// succeeds.
///
/// `reply_deadline` bounds ONLY steps 4-5 (sending `status` and reading
/// its reply) — steps 1-3 are local, synchronous OS calls with no wait to
/// bound. Picking the deadline VALUE (the ADR's "2s, clamped to the
/// episode's remaining wall time") is the probe classifier's job, a
/// later unit; this function only enforces whatever it is given.
pub fn challenge(conn: &dyn ChallengeableConnection, reply_deadline: Instant) -> ChallengeOutcome {
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

    // Steps 4-5.
    let (reply_pid, reply_created) = match status_reply(conn, reply_deadline) {
        Ok(v) => v,
        Err(StatusFailure::Foreign) => return ChallengeOutcome::Foreign,
        Err(StatusFailure::Undetermined) => return ChallengeOutcome::Undetermined,
    };
    let created_now = match creation_filetime_bits(handle.as_raw_handle() as HANDLE) {
        Ok(c) => c,
        Err(_) => return ChallengeOutcome::Undetermined,
    };
    if reply_pid != server_pid || reply_created != created_now {
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

/// Send `status` and read its reply, bounded by `deadline` via a polling
/// watchdog that cancels the connection if the exchange is still
/// outstanding past it — connections implementing
/// [`ChallengeableConnection`] have no per-call timeout parameter of
/// their own, only a cross-thread `cancel`.
fn status_reply(
    conn: &dyn ChallengeableConnection,
    deadline: Instant,
) -> Result<(u32, u64), StatusFailure> {
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if Instant::now() >= deadline {
                    conn.cancel();
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        let result = (|| {
            let request = wire::encode_mgmt_request(&MgmtRequest::Status)
                .map_err(|_| StatusFailure::Undetermined)?;
            conn.write_all(&request).map_err(|_| StatusFailure::Undetermined)?;

            let mut splitter = wire::FrameSplitter::new();
            let mut buf = [0u8; 512];
            loop {
                let n = conn.read(&mut buf).map_err(|_| StatusFailure::Undetermined)?;
                if n == 0 {
                    return Err(StatusFailure::Undetermined); // ordered EOF mid-challenge
                }
                let (frames, err) = splitter.feed(&buf[..n]);
                if let Some(frame) = frames.into_iter().next() {
                    return match frame {
                        DecodedFrame::MgmtReply(MgmtReply::StatusOk { pid, created, .. }) => {
                            Ok((pid, created))
                        }
                        _ => Err(StatusFailure::Foreign), // well-formed, wrong opcode
                    };
                }
                if err.is_some() {
                    return Err(StatusFailure::Foreign); // undecodable / over the wire cap
                }
                // else: a partial frame — keep reading.
            }
        })();

        done.store(true, Ordering::Release);
        result
    })
}

// `pub(crate)`, not private: `probe.rs`'s own tests reuse
// `self_proven_process` below (via `crate::challenge::tests::...`) as
// their "give me a real `ChallengedProcess`" helper, rather than
// duplicating the bind-connect-reply dance a second time.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::pipe_win::{connect_voyage_pipe, PipeServer, TransportEvent};
    use crate::wire::Survival;

    fn fresh_voyage_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }

    fn expect_accepted(server: &PipeServer, timeout: Duration) -> crate::pipe_win::ConnId {
        match server.events().recv_timeout(timeout) {
            Ok(TransportEvent::Accepted(id)) => id,
            other => panic!("expected Accepted within {timeout:?}, got {other:?}"),
        }
    }

    /// The `status` request has no body, so its ENCODED length alone is
    /// what we wait for; the pipe is byte-type, so a single write is not
    /// guaranteed to surface as a single `Bytes` event.
    fn await_status_request(server: &PipeServer, conn_id: crate::pipe_win::ConnId) {
        let expected = wire::encode_mgmt_request(&MgmtRequest::Status).unwrap();
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.len() < expected.len() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for the status request");
            match server.events().recv_timeout(remaining) {
                Ok(TransportEvent::Bytes(cid, bytes)) if cid == conn_id => got.extend(bytes),
                Ok(other) => panic!("unexpected event waiting for status: {other:?}"),
                Err(_) => panic!("timed out waiting for the status request"),
            }
        }
        assert_eq!(got, expected);
    }

    fn self_pid_and_created() -> (u32, u64) {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId};
        let pid = unsafe { GetCurrentProcessId() };
        let created = creation_filetime_bits(unsafe { GetCurrentProcess() }).unwrap();
        (pid, created)
    }

    /// Real challenge, real pipe, SAME process on both ends — a genuine
    /// same-user server, proven. Also this test's own shared "give me a
    /// real `ChallengedProcess`" helper (`probe.rs`'s scripted-ops smoke
    /// test reuses it): it holds a handle to THIS TEST PROCESS, so
    /// callers must never call `.terminate()` on the result.
    pub(crate) fn self_proven_process() -> ChallengedProcess {
        let voyage_id = fresh_voyage_id();
        let server = PipeServer::bind(&voyage_id, 1).expect("bind");
        let client = connect_voyage_pipe(&voyage_id).expect("connect");

        std::thread::scope(|scope| {
            let challenge_handle =
                scope.spawn(|| challenge(&client, Instant::now() + Duration::from_secs(5)));

            let conn_id = expect_accepted(&server, Duration::from_secs(5));
            await_status_request(&server, conn_id);
            let (pid, created) = self_pid_and_created();
            let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk {
                pid,
                created,
                survival: Survival::Normal,
            })
            .unwrap();
            server.send(conn_id, reply, None).expect("send status_ok");

            match challenge_handle.join().expect("challenge thread panicked") {
                ChallengeOutcome::Proven(p) => p,
                other => panic!("expected Proven, got {other:?}"),
            }
        })
    }

    #[test]
    fn challenge_proves_a_genuine_same_user_server() {
        let p = self_proven_process();
        let (pid, created) = self_pid_and_created();
        assert_eq!(p.pid(), pid);
        assert_eq!(p.created(), created);
    }

    #[test]
    fn challenged_process_reverify_and_wait_reflect_a_live_self_proof() {
        let p = self_proven_process();
        assert!(p.reverify().unwrap());
        // Still running: this handle names our OWN test process.
        assert!(!p.wait(Duration::from_millis(50)).unwrap());
    }

    #[test]
    fn challenge_rejects_a_pid_creation_mismatch_as_foreign() {
        let voyage_id = fresh_voyage_id();
        let server = PipeServer::bind(&voyage_id, 1).expect("bind");
        let client = connect_voyage_pipe(&voyage_id).expect("connect");

        let outcome = std::thread::scope(|scope| {
            let challenge_handle =
                scope.spawn(|| challenge(&client, Instant::now() + Duration::from_secs(5)));

            let conn_id = expect_accepted(&server, Duration::from_secs(5));
            await_status_request(&server, conn_id);
            // A well-formed status_ok, but a FABRICATED pid/creation that
            // does not match the real server process (this test binary
            // itself) — the SID check upstream cannot catch this: same
            // account, wrong reply.
            let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk {
                pid: 1,
                created: 0,
                survival: Survival::Normal,
            })
            .unwrap();
            server.send(conn_id, reply, None).expect("send status_ok");

            challenge_handle.join().expect("challenge thread panicked")
        });

        assert!(matches!(outcome, ChallengeOutcome::Foreign), "{outcome:?}");
    }

    #[test]
    fn challenge_classifies_connection_death_mid_challenge_as_undetermined() {
        let voyage_id = fresh_voyage_id();
        let server = PipeServer::bind(&voyage_id, 1).expect("bind");
        let client = connect_voyage_pipe(&voyage_id).expect("connect");

        let outcome = std::thread::scope(|scope| {
            let challenge_handle =
                scope.spawn(|| challenge(&client, Instant::now() + Duration::from_secs(5)));

            let conn_id = expect_accepted(&server, Duration::from_secs(5));
            await_status_request(&server, conn_id);
            // The server closes without ever answering `status`.
            server.close(conn_id);

            challenge_handle.join().expect("challenge thread panicked")
        });

        assert!(matches!(outcome, ChallengeOutcome::Undetermined), "{outcome:?}");
    }
}
