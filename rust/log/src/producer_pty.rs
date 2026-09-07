//! `impl Producer for PtyProducer` — a bare Unix `openpty` + process group
//! behind the [`crate::producer::Producer`] trait (ADR 0043 "Decisions for
//! LU2" LU2b), the Unix twin of `producer_conpty.rs`'s `ConptyProducer`.
//! `spawn`'s `pre_exec` body is `capsule_legacy.rs`'s own `spawn_on_pty`
//! carried over VERBATIM (decision 14): new session, slave becomes the
//! controlling tty, stdio duped onto it, every inherited fd ≥ 3 closed
//! before exec (the flock rationale is unchanged — see the comment at the
//! call site). Two things are genuinely NEW here, both decision-driven:
//!
//! - **The held-EOF gate (decision 12).** A ConPTY's output handle stays
//!   open regardless of the child's lifetime; a Unix pty master reports the
//!   child's death (`Ok(0)` or `EIO` — the kernel's choice between the two
//!   is unspecified, and both mean exactly the same thing: the last slave
//!   fd closed) the INSTANT it happens, long before this loop ever calls
//!   [`close_output_side`](Producer::close_output_side). Rather than a
//!   platform flag that turns "pre-close EOF is fatal" on or off for this
//!   one platform, [`HeldEofReader`] HOLDS that terminal read behind a
//!   flag + condvar released only when `close_output_side` actually runs —
//!   so `producer.rs`'s own universal contract ("EOF only after the loop
//!   closed it") needs no per-platform exception at all.
//! - **The kill domain is the process group (decision 14).** `setsid()` in
//!   `pre_exec` already makes the child a new session AND process-group
//!   leader whose pgid equals its own pid, so [`killpg`] on that pid reaches
//!   the whole domain (a plain child of the pty child stays in the SAME
//!   group unless it calls `setpgid`/`setsid` itself — the same documented
//!   carve-out ConPTY's own broker has). `PR_SET_PDEATHSIG(SIGKILL)` closes
//!   the other half: a hard-killed capsule (its main thread — the one that
//!   called [`Producer::spawn`] — dying by any means, including `SIGKILL`)
//!   must never orphan the producer either.

#![cfg(unix)]

use crate::producer::{ExitStatus, Producer};
use crate::{Error, Result};
use serde_json::json;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// `PR_SET_PDEATHSIG` (`include/uapi/linux/prctl.h`) — NOT exposed by this
/// crate's pinned `libc` for a plain glibc/musl Linux target (only its
/// `android`/`fuchsia`/`l4re` modules declare it), so this crate defines
/// the value locally — the same device `challenge_unix.rs` already uses
/// for `SO_PEERPIDFD`/`PIDFD_INFO_EXIT` (ADR 0043 decision 8's own
/// precedent): a stable UAPI constant, not something that varies by
/// architecture.
#[cfg(target_os = "linux")]
const PR_SET_PDEATHSIG: libc::c_int = 1;

/// `PtyProducer::spawn`'s own pre-flight check (see that call site's own
/// doc for WHY this exists — the `close_range` in `pre_exec` closes
/// `std::process::Command`'s own exec-failure-reporting pipe before a
/// real `execve` failure could ever be reported through it): true if
/// `program` resolves to a file `access(2)` reports as executable by
/// THIS process — a PATH search (mirroring `execvp`'s own algorithm) if
/// `program` has no `/`, a direct check otherwise. `access`, not a raw
/// stat+mode check: it correctly accounts for the calling process's
/// actual credentials (uid/gid, ACLs), the same test `execve` itself
/// performs, not merely the file's raw permission bits.
fn executable_is_resolvable(program: &str) -> bool {
    if program.contains('/') {
        return is_executable_file(std::path::Path::new(program));
    }
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| is_executable_file(&dir.join(program)))
}

fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(test)]
mod executable_is_resolvable_tests {
    use super::executable_is_resolvable;

    #[test]
    fn a_real_program_resolves_both_by_path_and_by_bare_name() {
        assert!(executable_is_resolvable("/bin/sh"), "/bin/sh must exist and be executable on every Linux CI image");
        assert!(executable_is_resolvable("sh"), "a bare name must resolve via PATH the same way execvp would");
    }

    #[test]
    fn a_nonexistent_program_never_resolves() {
        assert!(!executable_is_resolvable("/nonexistent/no_such_exe_lu2b"));
        assert!(!executable_is_resolvable("no_such_exe_lu2b_bare_9f31"));
    }

    #[test]
    fn a_non_executable_regular_file_does_not_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not_executable");
        std::fs::write(&path, b"not a real program").unwrap();
        assert!(!executable_is_resolvable(path.to_str().unwrap()));
    }
}

/// Shared release gate for [`HeldEofReader`] (ADR 0043 decision 12): a
/// flag + condvar, released exactly once by [`PtyProducer::close_output_side`].
/// `pub` (not merely `pub(crate)`): [`HeldEofReader::for_test`] and this
/// type's own constructor are the seam `tests/capsule.rs`'s
/// `held_eof_is_released_by_close` drives directly, with a plain pipe and
/// no real pty/child at all — the ONE property this pair owns needs
/// neither.
pub struct EofGate {
    released: Mutex<bool>,
    cv: Condvar,
}

impl EofGate {
    pub fn new() -> Self {
        Self { released: Mutex::new(false), cv: Condvar::new() }
    }

    /// Idempotent: a second call is a harmless no-op (already `true`,
    /// `notify_all` over no waiters does nothing) — see
    /// [`Producer::close_output_side`]'s own doc for why this producer's
    /// implementation is genuinely idempotent, unlike the Windows one.
    pub fn release(&self) {
        let mut g = self.released.lock().unwrap();
        *g = true;
        self.cv.notify_all();
    }

    fn wait_for_release(&self) {
        let mut g = self.released.lock().unwrap();
        while !*g {
            g = self.cv.wait(g).unwrap();
        }
    }
}

impl Default for EofGate {
    fn default() -> Self {
        Self::new()
    }
}

/// The Unix pty master's own read side, wrapped to satisfy [`Producer`]'s
/// universal EOF contract (ADR 0043 decision 12) — see this module's own
/// doc for the full rationale. `EINTR` retries the underlying read; every
/// OTHER error passes straight through, unheld — only the two shapes that
/// mean "the child is gone" (`Ok(0)`, or `EIO`, the normal Linux signal
/// that the last slave fd closed) are ever held behind [`EofGate`].
pub struct HeldEofReader {
    file: File,
    gate: Arc<EofGate>,
}

impl HeldEofReader {
    /// Test-only direct construction over an arbitrary read fd + gate.
    /// Production code only ever gets one through
    /// [`Producer::take_output`]; `tests/capsule.rs`'s own
    /// `held_eof_is_released_by_close` drives the held-EOF property
    /// directly against a plain pipe, needing neither a real pty nor a
    /// real child for it.
    pub fn for_test(file: File, gate: Arc<EofGate>) -> Self {
        Self { file, gate }
    }
}

impl Read for HeldEofReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.file.read(buf) {
                Ok(0) => {
                    self.gate.wait_for_release();
                    return Ok(0);
                }
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    self.gate.wait_for_release();
                    return Ok(0);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// [`Child`] plus the ONE `std::process::ExitStatus` [`Producer::wait`]
/// may ever observe — cached the first time `try_wait` reports it
/// (`Child::try_wait` itself requires `&mut self`, and every trait method
/// that touches it takes `&self`, so this whole thing lives behind a
/// `Mutex`, exactly as `capsule.rs`'s own `OutputBudget` state does for
/// the identical "shared, mutation-needing state behind a `&self`
/// method" reason).
struct ChildState {
    child: Child,
    status: Option<std::process::ExitStatus>,
}

/// One producer under a Unix pty — `openpty` for the terminal, a plain
/// process group (via `setsid` in `pre_exec`) as the kill domain. See the
/// module doc for the two decision-driven pieces (`HeldEofReader`, the
/// process-group domain); everything else mirrors `capsule_legacy.rs`'s
/// own `spawn_on_pty`/`PtyChild` almost verbatim.
pub struct PtyProducer {
    writer: File,
    reader_fd: Option<OwnedFd>,
    gate: Arc<EofGate>,
    state: Mutex<ChildState>,
    /// The child's own pid, captured once at spawn — ALSO its process
    /// GROUP id (`setsid` in `pre_exec` makes the child both a session
    /// AND process-group leader), so `terminate_domain`/`domain_is_empty`
    /// can `killpg` on it without needing the state mutex at all.
    pid: libc::pid_t,
}

impl Producer for PtyProducer {
    type Output = HeldEofReader;

    fn pre_spawn_detail() -> serde_json::Value {
        // Decision 11: Unix contributes nothing to `producer_spawn.detail`
        // (Windows's `spawning_process_was_jobbed` has no Unix analogue).
        json!({})
    }

    fn spawn(argv: &[String], cols: u16, rows: u16) -> Result<Self> {
        if argv.is_empty() {
            return Err(Error::State("capsule argv is empty".into()));
        }
        // A DELIBERATE, DOCUMENTED deviation from a literal "just call
        // `Command::spawn`" implementation (reported per the brief's own
        // instruction for an impossible-as-written combination): the
        // `close_range` call in `pre_exec` below (carried over verbatim
        // from `capsule_legacy.rs`, for the flock race its own comment
        // documents) closes EVERY fd >= 3 in the CHILD before `execve` is
        // ever attempted — including the anonymous, `CLOEXEC`-marked pipe
        // `std::process::Command`'s OWN fallback fork+exec path uses to
        // report a POST-FORK failure (a failed `execve` included) back to
        // this call's `Result`, at whatever fd number it happens to land
        // on (an unavoidable, undocumented implementation detail no
        // `pre_exec` closure can see or protect). Verified empirically
        // (a minimal repro: `Command::new("/nonexistent").pre_exec(||
        // { close every fd 3..256; Ok(()) })` closes it first here too):
        // with the pipe closed, a real `execve` failure never reaches the
        // parent as `Err` at all -- the child instead aborts (`SIGABRT`)
        // when `std`'s own error-reporting `write` fails its internal
        // assertion, which `wait()` observes as an ordinary (if violent)
        // process exit, not a spawn failure. Neither property may be
        // silently dropped: the flock fix is real (a rare parallel-test
        // flake, per its own comment) and `ExitKind::SpawnFailed` must
        // stay honest (`tests/capsule.rs`'s own
        // `spawn_failure_is_compensated_unix`, the Unix twin of the
        // Windows spawn-failure test). The fix keeps BOTH: resolve
        // argv[0] (PATH search, exactly `execvp`'s own algorithm, if it
        // has no `/`) and probe it with `access(X_OK)` -- the same check
        // `execve` itself performs -- BEFORE ever forking, so the common
        // "doesn't exist" / "not executable" cases are caught here,
        // honestly, synchronously, with `close_range` never in the
        // picture for them at all. This does not close every possible
        // `execve`-time race (a TOCTOU deletion between this check and
        // the fork, or an exotic exec-time failure like a corrupt ELF
        // header) -- those residual cases would still surface as the
        // aborted-child shape above, exactly as they did before this
        // fix, but they are far rarer than "the path is simply wrong,"
        // which is what every realistic caller (and this crate's own
        // test) actually exercises.
        if !executable_is_resolvable(&argv[0]) {
            return Err(Error::Io(io::Error::from_raw_os_error(libc::ENOENT)));
        }
        let mut master_fd: libc::c_int = -1;
        let mut slave_fd: libc::c_int = -1;
        // Geometry at spawn (the loop already validated it, 2x2..512x256 —
        // `Producer::spawn`'s own doc). `winsz` is passed as `&mut` so the
        // SAME call site satisfies both `openpty`'s Linux signature
        // (`winp: *const winsize`) and its non-Linux-unix one (`*mut
        // winsize`, e.g. macOS/BSD) — a `&mut T` coerces to either.
        let mut winsz = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut winsz,
            )
        };
        if rc != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        let slave_raw = slave.as_raw_fd();
        // SAFETY: this closure runs on the forked child, between fork and
        // exec — only async-signal-safe calls, per `pre_exec`'s own
        // contract. Every call below is.
        unsafe {
            cmd.pre_exec(move || {
                // Verbatim from `capsule_legacy.rs`'s own `spawn_on_pty`
                // (ADR 0043 decision 14 carries it into this producer
                // as-is): new session; slave becomes the controlling tty;
                // stdio on it.
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                for fd in 0..=2 {
                    if libc::dup2(slave_raw, fd) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                // Close EVERY inherited fd ≥ 3 before exec. O_CLOEXEC
                // alone is not enough: between fork and exec this child
                // holds copies of all parent fds, and a flock lives on
                // the open file description — so a capsule-host thread
                // dropping and reopening a voyage lock during this window
                // would collide with its own lock through us (observed as
                // a rare parallel-test flake). `close_range` severs those
                // references at the earliest point.
                #[cfg(target_os = "linux")]
                {
                    if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                // Non-Linux unix (ADR 0043 "Decisions for LU2" LU2b): no
                // `close_range` syscall exists, so a bounded loop from fd
                // 3 to the process's own descriptor-table limit does the
                // same job — a `close` on an fd that was never open is a
                // harmless `EBADF`, ignored (mirrors `close_range`'s own
                // "gaps are fine" semantics).
                #[cfg(not(target_os = "linux"))]
                {
                    let limit = libc::getdtablesize();
                    if limit > 3 {
                        for fd in 3..limit {
                            libc::close(fd);
                        }
                    }
                }
                // ADR 0043 decision 14: a hard-killed capsule must never
                // orphan its producer — fires on the death of the
                // SPAWNING THREAD (this `pre_exec` body runs on a forked
                // copy of the loop's own main thread, the only thread
                // that ever calls `spawn`), not merely the process, per
                // `prctl(2)`'s own documented semantics. Linux-only: no
                // portable non-Linux-unix equivalent exists.
                #[cfg(target_os = "linux")]
                {
                    if libc::syscall(
                        libc::SYS_prctl,
                        PR_SET_PDEATHSIG as libc::c_long,
                        libc::SIGKILL as libc::c_long,
                        0i64,
                        0i64,
                        0i64,
                    ) < 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(Error::Io)?;
        drop(slave); // parent keeps only the master-side fds

        let writer = File::from(master.try_clone().map_err(Error::Io)?);
        let pid = child.id() as libc::pid_t;
        Ok(Self {
            writer,
            reader_fd: Some(master),
            gate: Arc::new(EofGate::new()),
            state: Mutex::new(ChildState { child, status: None }),
            pid,
        })
    }

    fn take_output(&mut self) -> Self::Output {
        let fd = self.reader_fd.take().expect("PtyProducer::take_output called twice");
        HeldEofReader { file: File::from(fd), gate: Arc::clone(&self.gate) }
    }

    fn input(&mut self) -> &mut dyn Write {
        &mut self.writer
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let winsz = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // Any fd referencing the master satisfies `TIOCSWINSZ` — the
        // property lives on the pty pair, not on which duplicate fd asks
        // for it — so the already-open write side needs no extra dup.
        let rc = unsafe { libc::ioctl(self.writer.as_raw_fd(), libc::TIOCSWINSZ as _, &winsz) };
        if rc != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        Ok(())
    }

    fn wait(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let mut guard = self.state.lock().unwrap();
                if guard.status.is_some() {
                    return Ok(true);
                }
                if let Some(status) = guard.child.try_wait().map_err(Error::Io)? {
                    guard.status = Some(status);
                    return Ok(true);
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn exit_status_after_confirmed_exit(&self) -> Result<ExitStatus> {
        let guard = self.state.lock().unwrap();
        let status = guard.status.expect(
            "PtyProducer::exit_status_after_confirmed_exit: precondition violated -- wait() must \
             have already confirmed exit",
        );
        if let Some(c) = status.code() {
            return Ok(ExitStatus::Code(c as u32));
        }
        if let Some(n) = status.signal() {
            return Ok(ExitStatus::Signal(n));
        }
        Err(Error::State(format!(
            "PtyProducer: exit status {status:?} carries neither a code nor a signal"
        )))
    }

    fn terminate_domain(&self) -> Result<()> {
        // ESRCH (decision 14): the domain is already empty -- a harmless
        // no-op, not a failure.
        if unsafe { libc::killpg(self.pid, libc::SIGKILL) } != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(Error::Io(err));
            }
        }
        Ok(())
    }

    fn domain_is_empty(&self) -> Result<bool> {
        let reaped = {
            let mut guard = self.state.lock().unwrap();
            if guard.status.is_some() {
                true
            } else if let Some(status) = guard.child.try_wait().map_err(Error::Io)? {
                guard.status = Some(status);
                true
            } else {
                false
            }
        };
        if !reaped {
            return Ok(false);
        }
        // sig=0: existence-only probe, no signal actually sent.
        if unsafe { libc::killpg(self.pid, 0) } == 0 {
            return Ok(false); // the group still has at least one live member
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(true)
        } else {
            Err(Error::Io(err))
        }
    }

    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        // No fd to close on Unix -- the slave was dropped at spawn. The
        // held-EOF release is the whole job, and it happens synchronously
        // here (never blocks); the spawned thread exists only to satisfy
        // this trait method's `JoinHandle` return shape.
        self.gate.release();
        std::thread::spawn(|| {})
    }
}

impl Drop for PtyProducer {
    fn drop(&mut self) {
        // Safety net (mirrors `Pseudoconsole::drop`'s own spirit): if the
        // child is still alive when this producer is dropped (a
        // panicking `run`, or any path that skipped ordinary teardown),
        // kill the whole process-group domain and reap it, so a
        // panicking `run` never leaks a live producer.
        let mut guard = self.state.lock().unwrap();
        if guard.status.is_none() {
            match guard.child.try_wait() {
                Ok(Some(status)) => guard.status = Some(status),
                Ok(None) => {
                    unsafe {
                        libc::killpg(self.pid, libc::SIGKILL);
                    }
                    let _ = guard.child.wait();
                }
                Err(_) => {}
            }
        }
    }
}

/// The inherited parent-death lease's own capsule-side check (ADR 0043
/// decision 15): the supervisor holds the pipe's WRITE end open and never
/// writes; this capsule inherited the READ end at a fixed fd and polls it
/// with exactly one non-blocking read. `EAGAIN`/`EWOULDBLOCK` (no data,
/// write end still open) is the ONLY "alive" outcome; anything else — a
/// graceful `Ok(0)` (write end closed), any other error, an fd that was
/// never open at all (`EBADF`), or even a byte actually read (the
/// supervisor's own contract says this never happens) — is broken, the
/// same "an unopenable name is reported identically to an
/// opened-but-broken one" contract [`crate::lease::open`] documents for
/// the Windows named-mutex sibling. `O_NONBLOCK` is set on the fd itself
/// (not merely the read call): the fd is a dedicated inherited descriptor
/// no other code touches, so mutating its flags carries no shared-state
/// risk.
pub fn parent_lease_fd_broken(fd: RawFd) -> bool {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return true; // not an open fd (EBADF), or another failure
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return true;
        }
        let mut byte = [0u8; 1];
        let rc = libc::read(fd, byte.as_mut_ptr() as *mut libc::c_void, 1);
        if rc < 0 {
            // `EAGAIN` and `EWOULDBLOCK` are the SAME value on Linux (a
            // single match arm covers both there, which is why this is
            // written as one comparison rather than an `|`-pattern that
            // would warn on the redundant second value on this target) —
            // written this way rather than `cfg`-splitting the two names
            // apart for a portability difference this crate's own other
            // Unix code (`socket_unix.rs`) doesn't bother distinguishing
            // either.
            return io::Error::last_os_error().raw_os_error() != Some(libc::EAGAIN);
        }
        // `rc == 0` (write end closed) or `rc > 0` (a byte actually read —
        // the supervisor's own contract says it never writes, so this is
        // an unexpected shape, not the one proven-alive signal) both fail
        // closed as broken.
        true
    }
}

#[cfg(test)]
mod parent_lease_tests {
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
        assert!(!parent_lease_fd_broken(r), "a pipe with its write end still open must read as alive");
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
        assert!(parent_lease_fd_broken(r), "a pipe whose write end closed must read as broken");
        unsafe {
            libc::close(r);
        }
    }

    #[test]
    fn broken_for_a_missing_or_closed_fd() {
        assert!(parent_lease_fd_broken(-1), "a negative fd must read as broken");
        // A definitely-closed fd number -- EBADF, not a real descriptor.
        let (r, w) = make_pipe();
        unsafe {
            libc::close(r);
            libc::close(w);
        }
        assert!(parent_lease_fd_broken(r), "a closed fd number must read as broken");
    }
}
