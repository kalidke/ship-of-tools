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
//!   exits immediately if it differs — the parent died in the fork-to-arm
//!   gap and this process was already reparented (to a subreaper) before
//!   arming could even run, so proceeding would leave an unsupervised
//!   producer running undetected. Everything else in `pre_exec` keeps its
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
//!   Linux scan). The leader is reaped EXACTLY ONCE, LAST: `Drop`
//!   (`killpg(SIGKILL)`, harmless if already dead, then a blocking,
//!   consuming `waitpid`) is the ONLY place that ever reaps — safe in
//!   every state, because until that reap runs the pgid stays pinned by
//!   the unreaped leader, alive or zombie.
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
        Ok(self.exit.lock().unwrap().expect(
            "PtyProducer::exit_status_after_confirmed_exit: precondition violated -- wait() must \
             have already confirmed exit",
        ))
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
    fn domain_is_empty(&self) -> Result<bool> {
        // Live-member scan (decision 13/14, review round): a zombie
        // leader (deliberately unreaped until `Drop`, per the module
        // doc) or a zombie descendant must NOT count against emptiness
        // -- only `/proc`'s own per-process state field distinguishes
        // "exited, awaiting reap" (`Z`) and the rarer post-exit "dead"
        // (`X`) from anything that could still run. Non-Linux Unix has
        // no portable equivalent scan; see this method's own sibling arm
        // below for the documented, coarser fallback there.
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
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{name}/stat")) else { continue };
            // `comm` (field 2) is parenthesized and may itself contain
            // spaces/parens -- find the LAST `)` first (same device
            // `challenge_unix.rs`'s own `/proc/pid/stat` parser uses),
            // then fields 3.. are single-space-separated from there.
            let Some(close) = stat.rfind(')') else { continue };
            let mut fields = stat[close + 1..].split_whitespace();
            let Some(state) = fields.next() else { continue }; // field 3
            // field 4 (ppid) is skipped by `.nth(1)`'s own counting;
            // field 5 (pgrp) is what we actually compare.
            let Some(pgrp) = fields.nth(1).and_then(|s| s.parse::<libc::pid_t>().ok()) else { continue };
            if pgrp == self.pid && state != "Z" && state != "X" {
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
        // The ONE place that ever reaps the leader (decision 13/14,
        // review round) -- safe in every state, because until this runs
        // the pgid stays PINNED by the unreaped leader, whether it is
        // still alive or already a zombie: `killpg` is a harmless no-op
        // (`ESRCH` is impossible here, since the pgid cannot yet have
        // vanished) if it is already a zombie, a real kill if any member
        // is still alive, and the blocking `waitpid` that follows is
        // what finally frees the pid for reuse.
        unsafe {
            libc::killpg(self.pid, libc::SIGKILL);
            let mut status: libc::c_int = 0;
            libc::waitpid(self.pid, &mut status, 0);
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

/// Direct, same-file tests against `PtyProducer` itself (the review
/// round's own repro shapes for the held-slave drop behavior and the
/// live-member domain scan) — these need `self.pid` and the `Producer`
/// trait's own methods directly, without a whole `capsule::run` loop
/// around them; `tests/capsule.rs`'s own `unix_only` module covers the
/// full-loop-level property (`output_after_a_slave_reopen_is_recorded`).
/// The two process-tree tests read `/proc` and are Linux-only (they ran on
/// the macOS CI leg once and failed for want of `/proc`); the reader-strand
/// test needs no `/proc` and runs on every Unix.
#[cfg(test)]
mod drop_and_domain_tests {
    use super::{Producer, PtyProducer};
    use std::io::Read;
    use std::time::Duration;
    #[cfg(target_os = "linux")]
    use std::time::Instant;

    #[cfg(target_os = "linux")]
    fn direct_children(pid: libc::pid_t) -> Vec<libc::pid_t> {
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect()
    }

    #[cfg(target_os = "linux")]
    fn proc_state(pid: libc::pid_t) -> Option<String> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let close = stat.rfind(')')?;
        stat[close + 1..].split_whitespace().next().map(str::to_string)
    }

    #[cfg(target_os = "linux")]
    fn proc_exists(pid: libc::pid_t) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    #[cfg(target_os = "linux")]
    fn wait_for_direct_child(pid: libc::pid_t, timeout: Duration) -> Option<libc::pid_t> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(&child) = direct_children(pid).first() {
                return Some(child);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
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

    #[cfg(target_os = "linux")]
    fn wait_until_gone(pid: libc::pid_t, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if !proc_exists(pid) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
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

    /// Review round, F3/F5/F6's own repro shape: after the LEADER has
    /// already exited (and this producer has deliberately NOT reaped it
    /// — see the module doc's third point), a surviving DESCENDANT in
    /// the same process group must still be killed by `Drop` alone, with
    /// no `terminate_domain`/`close_output_side` call ever made. `trap ''
    /// HUP` on the backgrounded `sleep` (inherited across its own exec,
    /// since `SIG_IGN` survives `exec` unlike a caught handler) is what
    /// lets it survive the leader's own exit at all — the leader, as a
    /// session leader with a controlling tty, would otherwise send it a
    /// real `SIGHUP` on exit, and the test would prove nothing about
    /// `Drop` specifically (mirrors `tests/e2e_socket.rs`'s identical
    /// finding, F7).
    #[test]
    #[cfg(target_os = "linux")]
    fn drop_kills_surviving_descendants_when_the_leader_already_exited() {
        // The `sleep 0.3` between backgrounding and exiting is load-
        // bearing, not padding: a child is reparented to a subreaper the
        // INSTANT its parent exits (not merely once the parent is later
        // reaped), so `wait_for_direct_child` below would otherwise race
        // the shell's own near-instant `exit 0` and could observe an
        // already-empty children list.
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "trap '' HUP; sleep 600 & sleep 0.3; exit 0".to_string(),
        ];
        let producer = PtyProducer::spawn(&argv, 80, 24).unwrap();

        let descendant_pid = wait_for_direct_child(producer.pid, Duration::from_secs(5))
            .expect("the shell never forked its backgrounded sleep within 5s");
        assert!(wait_for_zombie(producer.pid, Duration::from_secs(5)), "the leader never exited within 5s");
        assert!(
            proc_exists(descendant_pid),
            "the backgrounded sleep must survive the leader's own exit (SIGHUP is ignored)"
        );

        drop(producer); // no terminate_domain/close_output_side call -- Drop alone must do this

        assert!(
            wait_until_gone(descendant_pid, Duration::from_secs(5)),
            "the surviving descendant was still alive 5s after Drop"
        );
    }

    /// Review round, F6's own repro shape: `domain_is_empty` must ignore
    /// BOTH a zombie leader and a zombie descendant. Backgrounds a
    /// short-lived `sleep 0.05`, then EXEC-replaces the leader itself
    /// (SAME pid, so its role as the descendant's real OS parent is
    /// unaffected) into a long-lived `sleep 600` that never calls `wait`
    /// on anything -- the short sleep's zombie therefore has NO reaper
    /// and persists deterministically until this test (or `Drop`) reaps
    /// the leader.
    #[test]
    #[cfg(target_os = "linux")]
    fn domain_is_empty_ignores_zombie_descendants() {
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 0.05 & exec sleep 600".to_string()];
        let producer = PtyProducer::spawn(&argv, 80, 24).unwrap();

        let descendant_pid = wait_for_direct_child(producer.pid, Duration::from_secs(5))
            .expect("the shell never forked its backgrounded sleep within 5s");
        assert!(
            wait_for_zombie(descendant_pid, Duration::from_secs(5)),
            "the short-lived descendant never became a zombie within 5s"
        );

        // The leader is still alive (now running as `sleep 600`) --
        // domain_is_empty must say so, DESPITE the zombie descendant
        // already present.
        assert!(!producer.domain_is_empty().unwrap(), "the leader is still alive; the domain is not empty");

        producer.terminate_domain().unwrap();
        assert!(
            wait_for_zombie(producer.pid, Duration::from_secs(5)),
            "the leader never became a zombie after terminate_domain within 5s"
        );

        assert!(
            producer.domain_is_empty().unwrap(),
            "a domain with only zombie members (leader AND descendant) must read as empty"
        );

        drop(producer); // reaps the leader; the orphaned zombie descendant is reparented and reaped by the ambient subreaper on its own time, outside this test's own concern
    }
}
