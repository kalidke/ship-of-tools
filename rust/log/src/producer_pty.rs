//! `impl Producer for PtyProducer` — a bare Unix `openpty` + process group
//! behind the [`crate::producer::Producer`] trait (ADR 0043 "Decisions for
//! LU2" LU2b, review round), the Unix twin of `producer_conpty.rs`'s
//! `ConptyProducer`. `spawn`'s `pre_exec` body is `capsule_legacy.rs`'s own
//! `spawn_on_pty` carried over VERBATIM (after its own new leading step —
//! see the second point below): new session, slave becomes the controlling
//! tty, stdio duped onto it, every inherited fd ≥ 3 closed before exec (the
//! flock rationale is unchanged — see the comment at the call site). Three
//! things are genuinely NEW here, all decision-driven; all three were
//! REVISED once by a Codex review round after the first landing (each
//! point below says what changed and why):
//!
//! - **The output side reports EOF only after the loop closes it, BY
//!   CONSTRUCTION (decision 12).** The first version held a flag+condvar
//!   gate over the master's own EIO/EOF, releasing it only when
//!   `close_output_side` ran. Review finding: `EIO` on the master means "no
//!   slave is open RIGHT NOW", not "the child is gone" — a child that
//!   closes its own stdio and reopens its controlling tty (legal, real
//!   producer behavior) yields `EIO` MID-RUN, and the gate then treated
//!   that first `EIO` as terminal, silently losing every byte written
//!   after the reopen (a verified voyage with zero producer frames, in the
//!   reviewer's own repro). FIX: the capsule itself keeps ONE slave
//!   `OwnedFd` (opened alongside the master in `spawn`, `CLOEXEC`, never
//!   read from or written to) for as long as the run lasts. With the
//!   capsule ALSO holding a slave open, the master never sees `EOF`/`EIO`
//!   while the child lives (or reopens its own tty) — it returns `Ok(0)`/
//!   `EIO` (the kernel's choice between the two is unspecified) exactly
//!   when the LAST slave closes, which is now [`close_output_side`]'s own
//!   job: it simply drops the held slave. `type Output` is a plain `File`
//!   (a dup of the master) — no gate, no wrapper type needed at all. `Drop`
//!   closes everything (the held slave, the write-side dup, and — see the
//!   third point — the reaped child), so an early return (a panicking
//!   `run`) can never strand a reader forever: there is no gate left to
//!   strand it on.
//! - **`PR_SET_PDEATHSIG` is armed FIRST, then the parent is verified
//!   (decision 14).** The first version armed it only at the END of
//!   `pre_exec`, after `setsid`/`TIOCSCTTY`/`dup2`/`close_range` — a real
//!   (if narrow) window in which the spawning THREAD could die before
//!   arming ever ran, leaving the child with no parent-death signal armed
//!   at all. FIX: `prctl(PR_SET_PDEATHSIG, SIGKILL)` is now the FIRST thing
//!   `pre_exec` does, closing that window; immediately after, it checks
//!   `getppid() == expected_ppid` (`expected_ppid` is `std::process::id()`,
//!   captured before `Command::spawn` and moved into the closure) and
//!   exits immediately if it differs. Review round 2 (R6): PDEATHSIG is
//!   documented (`prctl(2)`) as tracking the death of the SPAWNING
//!   THREAD specifically, not the whole process — in THIS binary the
//!   spawner is always the process's own MAIN thread (`spawn` is never
//!   called from any other thread), so "the spawning thread dies" and
//!   "the process exits" are the same event here, and the `getppid`
//!   check is what covers the fork-to-arm race for THAT event (the
//!   parent process having already exited, reparenting this one, before
//!   arming could take effect). A spawning THREAD dying alone, with the
//!   process itself surviving on another thread, is a real state
//!   `prctl(2)` distinguishes in general — it is simply not a state this
//!   binary's own architecture can ever produce, so it is not one this
//!   check needs to detect. Everything else in `pre_exec` keeps its
//!   original order, unchanged, after this new leading step.
//! - **Exit is OBSERVED without reaping; the leader is reaped EXACTLY ONCE,
//!   LAST, in `Drop`; domain emptiness is judged by LIVE members, never by
//!   reaped-ness (decision 13/14).** The first version's `wait` called
//!   `Child::try_wait`, which REAPS on success — freeing the pid
//!   immediately, so a LATER `killpg`/`domain_is_empty` call could silently
//!   address a RECYCLED pid in an unrelated process group; and a leader
//!   that had already exited (and been reaped) made `Drop` skip `killpg` on
//!   the theory that "the child isn't running", stranding any surviving
//!   descendant in the same group. Separately, the old `domain_is_empty`'s
//!   `killpg(pgid, 0) == ESRCH` check read a group containing only zombie
//!   descendants (dead, but not yet reaped by their own parent) as "not
//!   empty", even though nothing left in it could ever run again. FIX:
//!   `wait` observes the leader's exit with `waitid(P_PID, pid, ..,
//!   WEXITED | WNOHANG | WNOWAIT)` — `WNOWAIT` is the whole point: it
//!   reports the exit WITHOUT consuming it, so the leader's pid (and its
//!   process-group id, which equals it) stays valid and PINNED against
//!   reuse for as long as this producer exists. The observed `si_code`/
//!   `si_status` are cached so `exit_status_after_confirmed_exit` can
//!   answer from that cache without ever reaping either. `domain_is_empty`
//!   (Linux) scans `/proc/*/stat` for any entry whose process group (field
//!   5) equals this producer's pgid AND whose state (field 3) is NOT
//!   `Z`(zombie)/`X`(dead) — empty iff none exist; a zombie leader or a
//!   zombie descendant is correctly excluded either way, matching the
//!   ConPTY twin's own "active processes == 0" meaning. Non-Linux Unix has
//!   no portable `/proc`-equivalent scan, so it falls back to the coarser
//!   `killpg(pgid, 0) == ESRCH` probe — a real, accepted gap documented at
//!   that arm's own doc (a zombie LEADER this producer has deliberately not
//!   yet reaped still answers that probe as "exists", unlike the precise
//!   Linux scan). Review round 2 refined the Linux scan further (see
//!   `domain_is_empty`'s own doc): a per-PROCESS state field alone can lie
//!   (a process whose main thread alone has exited still shows `Z` while
//!   its worker threads run; a non-UTF-8 `comm` byte used to make the
//!   whole entry unreadable and get silently skipped) — the scan now
//!   reads `/proc/*/stat` as raw bytes and judges each member's liveness
//!   by its OWN TASKS, not its own single state field. The leader is
//!   reaped in `Drop`, and ONLY there: `killpg(SIGKILL)` (harmless if
//!   already dead), then a BOUNDED, `EINTR`-retrying, non-blocking
//!   (`WNOHANG`) poll of `waitpid` — never the original version's raw
//!   blocking call, which review round 2 found discarded `EINTR`
//!   entirely (a non-restarting signal handler anywhere in the process
//!   made the child NEVER get reaped, reproduced on every run) and could
//!   hang the whole capsule's own exit indefinitely on a leader stuck in
//!   an uninterruptible kernel wait. `Drop` is safe to run in every
//!   state, because until its own reap succeeds (or its bound expires)
//!   the pgid stays pinned by the unreaped leader, alive or zombie.
//!
//! The kill domain is still the PROCESS GROUP: `setsid()` in `pre_exec`
//! makes the child both a session AND process-group leader whose pgid
//! equals its own pid, so `killpg` on that pid reaches the whole domain (a
//! plain child of the pty child stays in the SAME group unless it calls
//! `setpgid`/`setsid` itself — the same documented carve-out ConPTY's own
//! broker has).

#![cfg(unix)]

use crate::producer::{ExitStatus, Producer};
use crate::{Error, Result};
use serde_json::json;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Mutex;
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

/// `Drop`'s own reap bound (review round 2, R3): after `killpg(SIGKILL)`,
/// only a task stuck in an uninterruptible kernel wait (`D` state — a
/// stuck NFS mount, say) can outlive a bounded, `WNOHANG`-polled reap
/// attempt. Leaving that one unreaped past this bound is safe: nothing
/// addresses this pgid again after `Drop` returns, and the exiting
/// capsule process hands the still-zombie-eventually child to its own
/// reaper (`init`/a subreaper) the same way any other unreaped child
/// would be.
const REAP_BOUND: Duration = Duration::from_secs(2);

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

/// One producer under a Unix pty — `openpty` for the terminal, a plain
/// process group (via `setsid` in `pre_exec`) as the kill domain. See the
/// module doc for the three decision-driven pieces (the held slave, PDEATHSIG
/// ordering, deferred reaping); everything else mirrors `capsule_legacy.rs`'s
/// own `spawn_on_pty`/`PtyChild` almost verbatim.
pub struct PtyProducer {
    writer: File,
    reader_fd: Option<OwnedFd>,
    /// The capsule's own held slave (decision 12, review round) — `CLOEXEC`,
    /// never read from or written to. Its PRESENCE, not its content, is
    /// the whole point: as long as this is `Some`, the master can never
    /// see `EOF`/`EIO`, regardless of what the child does with its own
    /// stdio. [`Producer::close_output_side`] sets this to `None`, which
    /// is the actual close(2) that finally lets the master's read side
    /// see the end of the stream.
    slave: Option<OwnedFd>,
    /// The leader's cached exit observation, populated by
    /// [`PtyProducer::observe_exit_without_reaping`] (`waitid(.., WNOWAIT)`
    /// — never a consuming wait). The one real reap is `Drop`'s — see the
    /// module doc's third point for why reaping is deferred that way.
    exit: Mutex<Option<ExitStatus>>,
    /// The child's own pid, captured once at spawn — ALSO its process
    /// GROUP id (`setsid` in `pre_exec` makes the child both a session
    /// AND process-group leader). Stays valid and meaningful for the
    /// whole lifetime of this producer, whether the leader is running or
    /// an unreaped zombie: `wait`'s own `WNOWAIT` observation, and the
    /// deferred single reap in `Drop`, are exactly what keeps this pid
    /// (and therefore the pgid `terminate_domain`/`domain_is_empty`
    /// address) pinned against reuse.
    pid: libc::pid_t,
}

impl Producer for PtyProducer {
    type Output = File;

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
        // F2 (review round): captured BEFORE `Command::spawn` and moved
        // into `pre_exec` — the child's own first act verifies against
        // THIS value (`getppid()` at that point), not against whatever
        // `getppid()` a later, racing read of `std::process::id()` might
        // return. Linux-only, matching the PDEATHSIG check itself: no
        // portable non-Linux-unix equivalent exists.
        #[cfg(target_os = "linux")]
        let expected_ppid = std::process::id() as libc::pid_t;
        // R2 (review round 2): establish the disposition the pid PIN
        // depends on, rather than assuming it. `wait`'s own
        // `waitid(.., WNOWAIT)` (see the module doc's third point) needs
        // a RETAINED zombie to observe; if `SIGCHLD` is `SIG_IGN` (or
        // `SA_NOCLDWAIT` is set) the kernel auto-reaps a terminated child
        // itself, with no zombie ever left to find (`waitid` then reports
        // `ECHILD`) — and the pgid this producer's own `Drop` later
        // signals could already have been recycled to something
        // unrelated. `SIG_IGN` is inherited across `exec` from ANY
        // supervisor this process happens to run under, so establishing
        // `SIG_DFL` here, ourselves, before the fork, is the only way to
        // be sure. `ECHILD` from `waitid` after this stays a real error
        // (`observe_exit_without_reaping`'s own doc): it would now mean a
        // FOREIGN reaper raced us, a genuine invariant violation, never
        // routine.
        if unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(Error::Io(io::Error::last_os_error()));
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
        // `openpty` hands back plain (non-CLOEXEC) descriptors: mark both
        // BEFORE the fork, so no other spawn from this process --
        // concurrent or later -- can inherit the pair. Our own child is
        // unaffected: `pre_exec` below `dup2`s the slave onto 0..=2 (which
        // clears the flag on those) and `close_range` severs its
        // inherited copies anyway. The parent keeps the slave open for
        // the whole run (decision 12).
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags >= 0 {
                unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
            }
        }

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        let slave_raw = slave.as_raw_fd();
        // SAFETY: this closure runs on the forked child, between fork and
        // exec — only async-signal-safe calls, per `pre_exec`'s own
        // contract. Every call below is.
        unsafe {
            cmd.pre_exec(move || {
                // F2 (review round): PDEATHSIG armed FIRST, before
                // anything else — closes the window in which the
                // spawning THREAD could die before arming ever ran. Then
                // verify the parent is STILL the one that forked us: if
                // `getppid()` differs from `expected_ppid`, the parent
                // already died in the fork-to-arm gap and this process
                // was reparented (to a subreaper) before arming could
                // even take effect against a live parent — exit rather
                // than run on, unsupervised and undetected.
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
                    if libc::getppid() != expected_ppid {
                        libc::_exit(1);
                    }
                }
                // Verbatim from `capsule_legacy.rs`'s own `spawn_on_pty`
                // (ADR 0043 decision 14 carries it into this producer
                // as-is, after the new leading step above): new session;
                // slave becomes the controlling tty; stdio on it.
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
                // references at the earliest point — including this
                // child's own inherited copies of the master and the
                // slave the PARENT is about to keep held (decision 12):
                // nothing from either leaks into the producer.
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
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(Error::Io)?;
        let pid = child.id() as libc::pid_t;
        // From here on this producer tracks the leader ITSELF, via raw
        // `waitid`/`waitpid` on `pid` — `Child`'s own `wait`/`try_wait`
        // are never called (they would REAP, which decision 13/14's own
        // fix specifically defers to `Drop`, exactly once, last). `Child`
        // holds no other resource worth keeping (no piped stdio was ever
        // requested), so dropping it here is inert.
        drop(child);

        let writer = File::from(master.try_clone().map_err(Error::Io)?);
        Ok(Self {
            writer,
            reader_fd: Some(master),
            slave: Some(slave),
            exit: Mutex::new(None),
            pid,
        })
    }

    fn take_output(&mut self) -> Self::Output {
        let fd = self.reader_fd.take().expect("PtyProducer::take_output called twice");
        File::from(fd)
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
                let mut guard = self.exit.lock().unwrap();
                if guard.is_some() {
                    return Ok(true);
                }
                if let Some(exit) = self.observe_exit_without_reaping()? {
                    *guard = Some(exit);
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
        // Review round 2 (R5): the trait's own doc allows confirmation
        // via EITHER `wait` or `domain_is_empty` -- a caller that
        // confirmed only through the latter would never have populated
        // this cache at all, and the first version of this method
        // panicked in that case. FIX: observe once, on demand, exactly
        // like `wait` itself does (`waitid(.., WNOWAIT)`, never
        // reaping); if that ALSO finds nothing (a genuine precondition
        // violation by the caller), return a loud `Err`, never panic.
        let mut guard = self.exit.lock().unwrap();
        if guard.is_none() {
            *guard = self.observe_exit_without_reaping()?;
        }
        guard.ok_or_else(|| {
            Error::State("PtyProducer: exit status requested before the leader's exit was confirmed".into())
        })
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

    #[cfg(target_os = "linux")]
    /// Live-member scan (decision 13/14; refined in review round 2, R1):
    /// a zombie leader (deliberately unreaped until `Drop`, per the
    /// module doc) or a zombie descendant must NOT count against
    /// emptiness — only `/proc`'s own per-TASK state field reliably
    /// distinguishes "exited, awaiting reap" (`Z`) and the rarer
    /// post-exit "dead" (`X`) from anything that could still run.
    /// Round 2 found two real false-empties in the first version (which
    /// used `read_to_string` and judged each MEMBER by its own single
    /// state field): (i) a process whose MAIN thread alone has exited
    /// (`pthread_exit`) shows `Z` in its own `/proc/<pid>/stat` while its
    /// worker threads keep running — judged live here iff ANY of its
    /// tasks (`/proc/<pid>/task/*/stat`) has a state that is not `Z`/`X`;
    /// (ii) a `comm` containing a non-UTF-8 byte made `read_to_string`
    /// fail outright, silently skipping the whole entry — fixed by
    /// reading every stat file as raw BYTES (`std::fs::read`) and only
    /// ever decoding the small numeric/single-byte fields this method
    /// actually needs, never `comm` itself. A stat file or task directory
    /// that vanishes mid-scan is simply one fewer member/task to find,
    /// not a failure. ASSUMPTION this method relies on: the capsule and
    /// its producer share ONE pid namespace and ONE `/proc` — true here
    /// because the producer is this process's own direct fork child, and
    /// `hidepid` (where configured) never hides a uid's own processes
    /// from itself. Non-Linux Unix has no portable equivalent scan; see
    /// this method's own sibling arm below for the documented, coarser
    /// fallback there.
    fn domain_is_empty(&self) -> Result<bool> {
        for entry in std::fs::read_dir("/proc").map_err(Error::Io)? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue; // not a pid directory (self, thread-self, etc.)
            }
            // A process may have exited between `read_dir`'s own listing
            // and this read -- that is simply one fewer member to find,
            // not a failure.
            let Ok(stat) = std::fs::read(format!("/proc/{name}/stat")) else { continue };
            let Some(mut fields) = stat_fields_from_field_3(&stat) else { continue };
            let Some(_state) = fields.next() else { continue }; // field 3 -- NOT trusted alone, see below
            let Some(_ppid) = fields.next() else { continue }; // field 4
            let Some(pgrp) = fields.next().and_then(parse_pid_field) else { continue }; // field 5
            if pgrp != self.pid {
                continue;
            }
            // This IS a member of our pgid -- its own (possibly
            // main-thread-only) state is not the last word; a live task
            // anywhere in the process counts.
            if any_task_is_live(name) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[cfg(not(target_os = "linux"))]
    fn domain_is_empty(&self) -> Result<bool> {
        // No portable `/proc`-equivalent scan exists here, so this is a
        // DOCUMENTED, coarser fallback (review round): `killpg(pgid, 0)`
        // only asks "does at least one process in this group exist",
        // which a zombie answers "yes" to (a zombie's pid/pgid stay
        // valid, just unable to receive real signals, until reaped) --
        // unlike the precise Linux scan above, a domain containing ONLY
        // an unreaped zombie leader (or zombie descendants) reads as
        // "not empty" here. Accepted for non-Linux Unix, which this
        // crate's own capsule support already treats as experimental
        // (ADR 0043 "Open for the maintainer" item 1).
        if unsafe { libc::killpg(self.pid, 0) } == 0 {
            return Ok(false);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(true)
        } else {
            Err(Error::Io(err))
        }
    }

    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        // Dropping the held slave (a real close(2) on the last owned
        // reference to it in THIS process) is what finally lets the
        // master observe EOF/EIO -- see the module doc's first point.
        // No actual blocking work happens here (unlike ConPTY's
        // `ClosePseudoConsole`); the spawned thread exists only to
        // satisfy this trait method's `JoinHandle` return shape.
        // Idempotent: a second call finds `None` already and does
        // nothing further.
        self.slave = None;
        std::thread::spawn(|| {})
    }
}

/// Splits a `/proc/<pid>/stat`-shaped byte buffer (identical format for
/// `/proc/<pid>/task/<tid>/stat`) into its whitespace-separated fields,
/// STARTING AT FIELD 3 (state) — `comm` (field 2) is parenthesized and
/// may itself contain spaces or parens, so this finds the LAST `)` first
/// (same device `challenge_unix.rs`'s own `/proc/pid/stat` parser uses)
/// and treats everything after it as field 3 onward. Operates on raw
/// BYTES throughout (review round 2, R1): `comm` itself is never
/// decoded, so a non-UTF-8 byte inside it can never make this fail.
/// `None` only if the buffer contains no `)` at all (a stat file that
/// vanished mid-read, or genuinely malformed).
#[cfg(target_os = "linux")]
fn stat_fields_from_field_3(stat: &[u8]) -> Option<impl Iterator<Item = &[u8]>> {
    let close = stat.iter().rposition(|&b| b == b')')?;
    Some(stat[close + 1..].split(|&b| b == b' ').filter(|f| !f.is_empty()))
}

/// Parses one `/proc/.../stat` numeric field (pid/ppid/pgrp are all the
/// same shape) from its raw bytes.
#[cfg(target_os = "linux")]
fn parse_pid_field(field: &[u8]) -> Option<libc::pid_t> {
    std::str::from_utf8(field).ok()?.parse().ok()
}

/// A member of our process group is live iff ANY of its tasks (kernel
/// threads) has a state that is neither `Z` (zombie) nor `X` (dead) —
/// see `domain_is_empty`'s own doc for why the process-level state field
/// alone is not trustworthy (a process whose MAIN thread alone has
/// exited still shows `Z` there while other tasks keep running). A task
/// directory or its `stat` file disappearing mid-scan (the whole process
/// exiting concurrently, say) is simply one fewer task to find, not a
/// failure — `pid_str`'s own task directory vanishing entirely reads as
/// "no live task", the same as "not found".
#[cfg(target_os = "linux")]
fn any_task_is_live(pid_str: &str) -> bool {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid_str}/task")) else {
        return false;
    };
    for task in tasks {
        let Ok(task) = task else { continue };
        let tid = task.file_name();
        let Some(tid) = tid.to_str() else { continue };
        let Ok(stat) = std::fs::read(format!("/proc/{pid_str}/task/{tid}/stat")) else { continue };
        let Some(mut fields) = stat_fields_from_field_3(&stat) else { continue };
        let Some(state) = fields.next() else { continue };
        if state != b"Z" && state != b"X" {
            return true;
        }
    }
    false
}

impl PtyProducer {
    /// Observes the leader's exit via `waitid(P_PID, pid, ..,
    /// WEXITED | WNOHANG | WNOWAIT)` — `WNOWAIT` is the whole point (see
    /// the module doc's third point): it reports the exit WITHOUT
    /// consuming it, so this producer's own `pid` (and the pgid, which
    /// equals it) stays valid until `Drop`'s own single, real reap.
    /// `Ok(None)`: no exit observed yet (POSIX: with `WNOHANG` and
    /// nothing to report, `si_pid` stays 0 — this local `siginfo_t` is
    /// zeroed before the call, so an unwritten field already reads as 0
    /// regardless of that guarantee). `Ok(Some(_))`: the leader has
    /// exited; `CLD_EXITED` maps to `Code`, `CLD_KILLED`/`CLD_DUMPED` to
    /// `Signal` — no other `si_code` is possible for a `WEXITED`-only
    /// request.
    fn observe_exit_without_reaping(&self) -> Result<Option<ExitStatus>> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        let status = unsafe { info.si_status() };
        match info.si_code {
            libc::CLD_EXITED => Ok(Some(ExitStatus::Code(status as u32))),
            libc::CLD_KILLED | libc::CLD_DUMPED => Ok(Some(ExitStatus::Signal(status))),
            other => Err(Error::State(format!(
                "PtyProducer: waitid reported an unexpected si_code {other} for a WEXITED-only wait"
            ))),
        }
    }
}

impl Drop for PtyProducer {
    fn drop(&mut self) {
        // The ONE place that ever reaps the leader (decision 13/14) --
        // safe to run in every state, because until this succeeds (or
        // its own bound expires) the pgid stays PINNED by the unreaped
        // leader, whether it is still alive or already a zombie:
        // `killpg` is a harmless no-op if it is already a zombie (it
        // cannot be signalled, but `ESRCH` is impossible here since the
        // pgid cannot yet have vanished), a real kill if any member is
        // still alive.
        unsafe {
            libc::killpg(self.pid, libc::SIGKILL);
        }
        // Review round 2 (R3): the FIRST version called a raw, blocking
        // `waitpid(.., 0)` and discarded its result -- a non-restarting
        // signal handler anywhere in this process turns that call into
        // `EINTR`, and a discarded `EINTR` means the child is NEVER
        // reaped (reproduced on every run of the reviewer's own repro).
        // Separately, a leader stuck in an uninterruptible kernel wait
        // (state `D`) can outlive `SIGKILL` entirely, which would block
        // this destructor -- and therefore the whole capsule's own exit
        // -- indefinitely. FIX: poll `waitpid(pid, WNOHANG)` every 10ms,
        // retrying `EINTR` immediately (no fresh delay needed -- the
        // signal was already handled by the time the syscall returned)
        // and treating `ECHILD` as "already reaped" (harmless: something
        // else — this same call, on a rare double-drop-adjacent race, or
        // a genuinely foreign reaper after R2's own `SIGCHLD` fix rules
        // out routine auto-reaping — already collected it), bounded by
        // `REAP_BOUND`. Past that bound, this destructor simply returns:
        // nothing ever addresses this pgid again after `Drop`, and the
        // exiting capsule process hands its own still-unreaped child to
        // ITS reaper the same way any other orphan would be.
        let deadline = Instant::now() + REAP_BOUND;
        loop {
            let mut status: libc::c_int = 0;
            let rc = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            if rc == self.pid {
                return; // reaped
            }
            if rc < 0 {
                match io::Error::last_os_error().raw_os_error() {
                    Some(libc::EINTR) => continue, // retry immediately
                    Some(libc::ECHILD) => return,  // already reaped
                    _ => return,                   // Drop cannot propagate an error; give up quietly
                }
            }
            // rc == 0: not yet reapable -- keep polling until the bound.
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
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
        // An fd number nothing in this test binary holds: descriptors are
        // allocated lowest-free-first, so the top of the table is never
        // reached. (Closing a fresh pipe and probing ITS number raced the
        // other test threads, which can reopen that number in between --
        // seen once as a flake in the lib suite.)
        let top = unsafe { libc::getdtablesize() } - 1;
        assert!(parent_lease_fd_broken(top), "an fd number that is not open must read as broken");
    }
}

/// Direct, same-file tests against `PtyProducer` itself (the review
/// round's own repro shapes for the held-slave drop behavior and the
/// live-member domain scan) — these need `self.pid` and the `Producer`
/// trait's own methods directly, without a whole `capsule::run` loop
/// around them; `tests/capsule.rs`'s own `unix_only` module covers the
/// full-loop-level property (`output_after_a_slave_reopen_is_recorded`).
/// The two process-tree tests read `/proc` and are Linux-only (they ran on
/// the macOS CI leg once and failed for want of `/proc`); the reader-strand
/// test needs no `/proc` and runs on every Unix.
///
/// Review round 2 (R7): the descendant-finding helper no longer reads the
/// LEADER's own `/proc/<pid>/task/<pid>/children` — that file empties out
/// the INSTANT the leader exits (a live child is reparented away right
/// then, not merely once the leader is later reaped), so it raced the
/// leader's own exit in the first version of these tests. Finding a
/// descendant by scanning ALL of `/proc` for a process whose OWN pgrp
/// equals the leader's pid works identically whether the leader is still
/// alive, already a zombie, or already reaped — a process's pgrp does not
/// change when its parent exits, only its ppid does — so no delay
/// between backgrounding a descendant and the leader's own exit is
/// needed anywhere below. Success for "the descendant is now dead" is
/// ALWAYS "state `Z`, or its `/proc` entry is gone" (`wait_until_dead`) —
/// never "gone" alone, which would depend on how fast an external
/// subreaper happens to reap it, not on anything this producer's own
/// `Drop` actually did.
#[cfg(test)]
mod drop_and_domain_tests {
    use super::{Producer, PtyProducer};
    use std::io::Read;
    use std::time::Duration;
    #[cfg(target_os = "linux")]
    use std::time::Instant;

    #[cfg(target_os = "linux")]
    fn proc_state(pid: libc::pid_t) -> Option<String> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let close = stat.rfind(')')?;
        stat[close + 1..].split_whitespace().next().map(str::to_string)
    }

    #[cfg(target_os = "linux")]
    fn wait_for_zombie(pid: libc::pid_t, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if proc_state(pid).as_deref() == Some("Z") {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// "Dead" here means EITHER a zombie (`Z`, awaiting reap by whoever
    /// its current parent is) OR fully gone (`/proc` entry absent) --
    /// never "gone" alone, which would depend on how fast some external
    /// subreaper happens to reap an orphan, not on what this producer's
    /// own `Drop` did.
    #[cfg(target_os = "linux")]
    fn wait_until_dead(pid: libc::pid_t, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match proc_state(pid) {
                Some(s) if s == "Z" => return true,
                None => return true, // /proc entry gone -- also dead
                Some(_) => {}        // still alive in some other state
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// One scan of `/proc` for a LIVE process whose OWN pgrp (field 5)
    /// equals `leader_pid` and whose pid is NOT `leader_pid` itself —
    /// see this module's own doc for why this replaces reading the
    /// leader's own `children` file.
    #[cfg(target_os = "linux")]
    fn find_descendant_by_pgrp(leader_pid: libc::pid_t) -> Option<libc::pid_t> {
        for entry in std::fs::read_dir("/proc").ok()? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(pid) = name.parse::<libc::pid_t>() else { continue };
            if pid == leader_pid {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{name}/stat")) else { continue };
            let Some(close) = stat.rfind(')') else { continue };
            let mut fields = stat[close + 1..].split_whitespace();
            let Some(_state) = fields.next() else { continue };
            let Some(_ppid) = fields.next() else { continue };
            let Some(pgrp) = fields.next().and_then(|s| s.parse::<libc::pid_t>().ok()) else { continue };
            if pgrp == leader_pid {
                return Some(pid);
            }
        }
        None
    }

    #[cfg(target_os = "linux")]
    fn wait_for_descendant_by_pgrp(leader_pid: libc::pid_t, timeout: Duration) -> Option<libc::pid_t> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pid) = find_descendant_by_pgrp(leader_pid) {
                return Some(pid);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Review round, F1/F4's own repro shape: a reader blocked on a
    /// `take_output()` `File` must NOT be stranded forever by an early
    /// `Drop` (a panicking `run`, or any path that skips
    /// `close_output_side`/teardown entirely). `sleep 600` as the
    /// producer: silent forever, so the reader's blocking `read` is
    /// GENUINELY still parked (not merely lucky timing) when we drop.
    #[test]
    fn drop_before_phase_b_does_not_strand_the_reader() {
        let mut producer = PtyProducer::spawn(&["sleep".to_string(), "600".to_string()], 80, 24).unwrap();
        let mut output = producer.take_output();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let _ = tx.send(output.read(&mut buf));
        });

        std::thread::sleep(Duration::from_millis(100));
        assert!(rx.try_recv().is_err(), "the reader must still be blocked before the early drop (sleep is silent)");

        drop(producer); // no close_output_side/terminate_domain call -- simulates an early return/panic

        let result = rx.recv_timeout(Duration::from_secs(10)).expect("the reader never unblocked after an early Drop");
        // An ordered EOF (`Ok(0)`) or an I/O error (`EIO`) are both an
        // acceptable "unblocked, not stranded" outcome -- Drop's own real
        // behavior (kill + reap the child, drop the held slave and the
        // write-side dup) is what actually severs the last reference;
        // which shape the kernel picks between the two is unspecified,
        // same as the module doc's own EOF/EIO note.
        if let Ok(n) = result {
            assert_eq!(n, 0, "expected an ordered EOF, got {n} bytes");
        }
        reader.join().unwrap();
    }

    /// Review round, F3/F5/F6's own repro shape (refined by round 2, R7):
    /// after the LEADER has already exited (and this producer has
    /// deliberately NOT reaped it — see the module doc's third point), a
    /// surviving DESCENDANT in the same process group must still be
    /// killed by `Drop` alone, with no `terminate_domain`/
    /// `close_output_side` call ever made. `trap '' HUP` on the
    /// backgrounded `sleep` (inherited across its own exec, since
    /// `SIG_IGN` survives `exec` unlike a caught handler) is what lets it
    /// survive the leader's own exit at all — the leader, as a session
    /// leader with a controlling tty, would otherwise send it a real
    /// `SIGHUP` on exit, and the test would prove nothing about `Drop`
    /// specifically (mirrors `tests/e2e_socket.rs`'s identical finding,
    /// F7). No delay between backgrounding and the shell's own `exit 0`
    /// is needed: `find_descendant_by_pgrp` finds the descendant by its
    /// OWN pgrp, which survives the leader's exit/reparenting untouched.
    #[test]
    #[cfg(target_os = "linux")]
    fn drop_kills_surviving_descendants_when_the_leader_already_exited() {
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), "trap '' HUP; sleep 600 & exit 0".to_string()];
        let producer = PtyProducer::spawn(&argv, 80, 24).unwrap();

        assert!(wait_for_zombie(producer.pid, Duration::from_secs(5)), "the leader never exited within 5s");
        let descendant_pid = wait_for_descendant_by_pgrp(producer.pid, Duration::from_secs(5))
            .expect("the backgrounded sleep must survive the leader's own exit (SIGHUP is ignored)");

        drop(producer); // no terminate_domain/close_output_side call -- Drop alone must do this

        assert!(
            wait_until_dead(descendant_pid, Duration::from_secs(5)),
            "the surviving descendant was still alive 5s after Drop"
        );
    }

    /// Review round 2 (R7): replaces the first round's
    /// `domain_is_empty_ignores_zombie_descendants`, which relied on an
    /// inherently transient state (a zombie descendant with no live
    /// members left at all — its own parent's eventual death reparents
    /// it to a reaper that may collect it at any time, so the window in
    /// which `domain_is_empty` could even be asked about it is not
    /// deterministic). The UNREAPED ZOMBIE LEADER is the deterministic
    /// case of the identical property this producer's own `domain_is_empty`
    /// must get right: a live leader means "not empty"; the SAME leader,
    /// killed and left an unreaped zombie (this producer's own contract
    /// -- see the module doc's third point), must read as "empty".
    #[test]
    #[cfg(target_os = "linux")]
    fn domain_is_empty_ignores_a_zombie_leader() {
        let producer = PtyProducer::spawn(&["sleep".to_string(), "600".to_string()], 80, 24).unwrap();

        assert!(!producer.domain_is_empty().unwrap(), "a live leader means the domain is not empty");

        producer.terminate_domain().unwrap();
        assert!(
            wait_for_zombie(producer.pid, Duration::from_secs(5)),
            "the leader never became a zombie after terminate_domain within 5s"
        );

        assert!(
            producer.domain_is_empty().unwrap(),
            "a domain containing only an unreaped zombie leader must read as empty"
        );
    }
}
