//! `impl Producer for PtyProducer` — a bare Unix `openpty` + process group
//! behind the [`crate::capsule::producer::Producer`] trait (ADR 0043 "Decisions for
//! LU2" LU2b), the Unix twin of `capsule/producer/conpty/producer.rs`'s
//! `ConptyProducer`. `spawn`'s `pre_exec` body
//! (after its own new leading step —
//! see the second point below): new session, slave becomes the controlling
//! tty, stdio duped onto it, every inherited fd ≥ 3 closed before exec (a
//! killed holder's lock copy and the pty fds — see the comment at the call site). Three
//! things are genuinely NEW here, all decision-driven:
//!
//! - **The output side reports EOF only after the loop closes it, BY
//!   CONSTRUCTION (decision 12).** The first version held a flag+condvar
//!   gate over the master's own EIO/EOF, releasing it only when
//!   `close_output_side` ran. `EIO` on the master means "no
//!   slave is open RIGHT NOW", not "the child is gone" — a child that
//!   closes its own stdio and reopens its controlling tty (legal, real
//!   producer behavior) yields `EIO` MID-RUN, and the gate then treated
//!   that first `EIO` as terminal, silently losing every byte written
//!   after the reopen (a verified voyage with zero producer frames).
//!   FIX: the capsule itself keeps ONE slave
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
//!   exits immediately if it differs. PDEATHSIG is
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
//! - **macOS gets NO twin of that arming dance, and needs no lease
//!   either — the pty hangup IS its parent-death mechanism.** The plan
//!   that reached this file asked for an inherited pipe here (parent
//!   holds the write end, child polls the read end for EOF). Two
//!   independent reasons it is not written. FIRST, it is not
//!   implementable AT THIS SITE at all: the "child" here is the agent
//!   binary itself — `claude`, a shell, whatever `producer_argv` names —
//!   third-party code that `exec` replaces us with, so nothing of ours
//!   survives to poll any descriptor. `PR_SET_PDEATHSIG` works precisely
//!   because it is a KERNEL property of the process that outlives `exec`
//!   and asks the child for no cooperation whatsoever; a lease would
//!   need a monitor process wedged between this one and the agent, which
//!   is a new process in every capsule tree. On macOS, closing the last master hangs up the controlling terminal and sends SIGHUP to its session. Our child
//!   takes that terminal with setsid and TIOCSCTTY. Both PTY ends receive checked close-on-exec flags, but concurrent
//!   creation-to-flagging inheritance remains possible; this producer is not unconditionally the sole master holder.
//!   The kernel-fact tests exercise last-master close with the slave released and held; neither proves absence of
//!   concurrent inheritance. SIGHUP is catchable, so a child ignoring it may outlive the hangup; reaping an orphaned
//!   group belongs to the supervisor.
//! - **Exit is OBSERVED without reaping; the leader is reaped EXACTLY ONCE,
//!   LAST, in `Drop`; domain emptiness is judged by LIVE members, never by
//!   reaped-ness (decision 13/14).**
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
//!   ConPTY twin's own "active processes == 0" meaning. macOS asks the
//!   SAME question through libproc (`proc_listpgrppids` +
//!   `PROC_PIDTBSDINFO.pbi_status`) rather than by any signalling errno:
//!   emptiness is judged by ONE mechanism and that mechanism ENUMERATES
//!   members, on both supported platforms. The coarser `killpg(pgid, 0)
//!   == ESRCH` probe non-Linux Unix once fell back to is DELETED, not
//!   widened — on Darwin its error is EPERM for a zombie-only group AND
//!   for a live member this uid may not signal, two answers a widening
//!   cannot separate, and taking it for "empty" would seal a capsule over
//!   a live descendant; a Unix that can enumerate neither now refuses the
//!   question, which `capsule::run`'s own fail-closed top already makes
//!   unreachable. A per-PROCESS state field alone can lie
//!   (a process whose main thread alone has exited still shows `Z` while
//!   its worker threads run; a non-UTF-8 `comm` byte used to make the
//!   whole entry unreadable and get silently skipped) — the scan now
//!   reads `/proc/*/stat` as raw bytes and judges each member's liveness
//!   by its OWN TASKS, not its own single state field. The leader is
//!   reaped in `Drop`, and ONLY there: `killpg(SIGKILL)` (harmless if
//!   already dead), then a BOUNDED, `EINTR`-retrying, non-blocking
//!   (`WNOHANG`) poll of `waitpid`. `Drop` is safe to run in every
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

use crate::capsule::producer::{ExitStatus, Producer};
use crate::{Error, Result};
use serde_json::json;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod verbs;

/// `PR_SET_PDEATHSIG` (`include/uapi/linux/prctl.h`) — NOT exposed by this
/// crate's pinned `libc` for a plain glibc/musl Linux target (only its
/// `android`/`fuchsia`/`l4re` modules declare it), so this crate defines
/// the value locally — the same device `challenge_unix.rs` already uses
/// for `SO_PEERPIDFD` (ADR 0043 decision 8's own precedent): a stable
/// UAPI constant, not something that varies by architecture.
#[cfg(target_os = "linux")]
const PR_SET_PDEATHSIG: libc::c_int = 1;

/// `Drop`'s own reap bound: after `killpg(SIGKILL)`,
/// only a task stuck in an uninterruptible kernel wait (`D` state — a
/// stuck NFS mount, say) can outlive a bounded, `WNOHANG`-polled reap
/// attempt. Leaving that one unreaped past this bound is safe: nothing
/// addresses this pgid again after `Drop` returns, and the exiting
/// capsule process hands the still-zombie-eventually child to its own
/// reaper (`init`/a subreaper) the same way any other unreaped child
/// would be.
const REAP_BOUND: Duration = Duration::from_secs(2);

/// One of the PTY factory's close-on-exec flag calls. A test may make it fail for real (the factory's own path then
/// runs) and learns which descriptors the factory flagged.
fn flag_fcntl(
    end: &'static str,
    fd: RawFd,
    cmd: libc::c_int,
    arg: libc::c_int,
) -> io::Result<libc::c_int> {
    #[cfg(test)]
    if flag_plan::fails(end, fd, cmd) {
        return Err(io::Error::from_raw_os_error(libc::EPERM));
    }
    #[cfg(not(test))]
    let _ = end;
    let rc = unsafe { libc::fcntl(fd, cmd, arg) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc)
}

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

/// One producer under a Unix pty — `openpty` for the terminal, a plain
/// process group (via `setsid` in `pre_exec`) as the kill domain. See the
/// module doc for the three decision-driven pieces (the held slave, PDEATHSIG
/// ordering, deferred reaping).
pub struct PtyProducer {
    writer: File,
    reader_fd: Option<OwnedFd>,
    /// The capsule's own held slave (decision 12) — `CLOEXEC`,
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

/// Splits a `/proc/<pid>/stat`-shaped byte buffer (identical format for
/// `/proc/<pid>/task/<tid>/stat`) into its whitespace-separated fields,
/// STARTING AT FIELD 3 (state) — `comm` (field 2) is parenthesized and
/// may itself contain spaces or parens, so this finds the LAST `)` first
/// (same device `challenge_unix.rs`'s own `/proc/pid/stat` parser uses)
/// and treats everything after it as field 3 onward. Operates on raw
/// BYTES throughout: `comm` itself is never
/// decoded, so a non-UTF-8 byte inside it can never make this fail.
/// `None` only if the buffer contains no `)` at all (a stat file that
/// vanished mid-read, or genuinely malformed).
#[cfg(target_os = "linux")]
fn stat_fields_from_field_3(stat: &[u8]) -> Option<impl Iterator<Item = &[u8]>> {
    let close = stat.iter().rposition(|&b| b == b')')?;
    Some(
        stat[close + 1..]
            .split(|&b| b == b' ')
            .filter(|f| !f.is_empty()),
    )
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
        let Ok(stat) = std::fs::read(format!("/proc/{pid_str}/task/{tid}/stat")) else {
            continue;
        };
        let Some(mut fields) = stat_fields_from_field_3(&stat) else {
            continue;
        };
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
        // A leader stuck in an uninterruptible kernel wait
        // (state `D`) can outlive `SIGKILL` entirely, which would block
        // this destructor -- and therefore the whole capsule's own exit
        // -- indefinitely. FIX: poll `waitpid(pid, WNOHANG)` every 10ms,
        // retrying `EINTR` immediately (no fresh delay needed -- the
        // signal was already handled by the time the syscall returned)
        // and treating `ECHILD` as "already reaped" (harmless: something
        // else — this same call, on a rare double-drop-adjacent race, or
        // a genuinely foreign reaper after `spawn`'s own `SIGCHLD` fix rules
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
                    _ => return, // Drop cannot propagate an error; give up quietly
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
/// opened-but-broken one" contract [`crate::supervisor::lease_win::open`] documents for
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
            // Unix code (`lane/socket_unix/`) doesn't bother distinguishing
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
mod executable_is_resolvable_tests {
    use super::executable_is_resolvable;

    #[test]
    fn a_real_program_resolves_both_by_path_and_by_bare_name() {
        assert!(
            executable_is_resolvable("/bin/sh"),
            "/bin/sh must exist and be executable on every Linux CI image"
        );
        assert!(
            executable_is_resolvable("sh"),
            "a bare name must resolve via PATH the same way execvp would"
        );
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
        assert!(
            !parent_lease_fd_broken(r),
            "a pipe with its write end still open must read as alive"
        );
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
        assert!(
            parent_lease_fd_broken(r),
            "a pipe whose write end closed must read as broken"
        );
        unsafe {
            libc::close(r);
        }
    }

    #[test]
    fn broken_for_a_missing_or_closed_fd() {
        assert!(
            parent_lease_fd_broken(-1),
            "a negative fd must read as broken"
        );
        // A definitely-closed fd number -- EBADF, not a real descriptor.
        // An fd number nothing in this test binary holds: descriptors are
        // allocated lowest-free-first, so the top of the table is never
        // reached. (Closing a fresh pipe and probing ITS number raced the
        // other test threads, which can reopen that number in between --
        // seen once as a flake in the lib suite.)
        let top = unsafe { libc::getdtablesize() } - 1;
        assert!(
            parent_lease_fd_broken(top),
            "an fd number that is not open must read as broken"
        );
    }
}

/// Direct, same-file tests against `PtyProducer` itself — these need `self.pid` and the `Producer`
/// trait's own methods directly, without a whole `capsule::run` loop
/// around them; `tests/capsule/`'s own `unix_only` module covers the
/// full-loop-level property (`output_after_a_slave_reopen_is_recorded`).
/// The two process-tree tests read `/proc` and are Linux-only (they ran on
/// the macOS CI leg once and failed for want of `/proc`); the reader-strand
/// test needs no `/proc` and runs on every Unix.
///
/// Finding a
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
        stat[close + 1..]
            .split_whitespace()
            .next()
            .map(str::to_string)
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
            let Ok(pid) = name.parse::<libc::pid_t>() else {
                continue;
            };
            if pid == leader_pid {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{name}/stat")) else {
                continue;
            };
            let Some(close) = stat.rfind(')') else {
                continue;
            };
            let mut fields = stat[close + 1..].split_whitespace();
            let Some(_state) = fields.next() else {
                continue;
            };
            let Some(_ppid) = fields.next() else { continue };
            let Some(pgrp) = fields.next().and_then(|s| s.parse::<libc::pid_t>().ok()) else {
                continue;
            };
            if pgrp == leader_pid {
                return Some(pid);
            }
        }
        None
    }

    #[cfg(target_os = "linux")]
    fn wait_for_descendant_by_pgrp(
        leader_pid: libc::pid_t,
        timeout: Duration,
    ) -> Option<libc::pid_t> {
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

    /// A reader blocked on a
    /// `take_output()` `File` must NOT be stranded forever by an early
    /// `Drop` (a panicking `run`, or any path that skips
    /// `close_output_side`/teardown entirely). `sleep 600` as the
    /// producer: silent forever, so the reader's blocking `read` is
    /// GENUINELY still parked (not merely lucky timing) when we drop.
    /// The leg's child sees the terminal the pty owner declares — read
    /// back through the pty itself, not from the producer's own process.
    /// Only the bytes are asserted (a reader thread, as the early-Drop
    /// test below): the child's exit is not part of the claim.
    #[test]
    fn the_child_sees_a_256_colour_truecolor_terminal() {
        let argv = ["/bin/sh", "-c", "printf '<%s|%s>' \"$TERM\" \"$COLORTERM\""].map(String::from);
        let mut producer = PtyProducer::spawn(&argv, 80, 24).unwrap();
        let mut output = producer.take_output();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut buf = [0u8; 256];
            loop {
                match output.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        seen.extend_from_slice(&buf[..n]);
                        if seen.ends_with(b">") {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&seen).into_owned());
        });
        let seen = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the pty never delivered the marker");
        assert!(
            seen.contains("<xterm-256color|truecolor>"),
            "pty output was {seen:?}"
        );
        drop(producer);
        reader.join().unwrap();
    }

    #[test]
    fn drop_before_phase_b_does_not_strand_the_reader() {
        let mut producer =
            PtyProducer::spawn(&["sleep".to_string(), "600".to_string()], 80, 24).unwrap();
        let mut output = producer.take_output();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let _ = tx.send(output.read(&mut buf));
        });

        std::thread::sleep(Duration::from_millis(100));
        assert!(
            rx.try_recv().is_err(),
            "the reader must still be blocked before the early drop (sleep is silent)"
        );

        drop(producer); // no close_output_side/terminate_domain call -- simulates an early return/panic

        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the reader never unblocked after an early Drop");
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

    /// After the LEADER has already exited (and this producer has
    /// deliberately NOT reaped it — see the module doc's third point), a
    /// surviving DESCENDANT in the same process group must still be
    /// killed by `Drop` alone, with no `terminate_domain`/
    /// `close_output_side` call ever made. `trap '' HUP` on the
    /// backgrounded `sleep` (inherited across its own exec, since
    /// `SIG_IGN` survives `exec` unlike a caught handler) is what lets it
    /// survive the leader's own exit at all — the leader, as a session
    /// leader with a controlling tty, would otherwise send it a real
    /// `SIGHUP` on exit, and the test would prove nothing about `Drop`
    /// specifically (mirrors `tests/e2e_socket/`'s identical finding,
    /// F7). No delay between backgrounding and the shell's own `exit 0`
    /// is needed: `find_descendant_by_pgrp` finds the descendant by its
    /// OWN pgrp, which survives the leader's exit/reparenting untouched.
    #[test]
    #[cfg(target_os = "linux")]
    fn drop_kills_surviving_descendants_when_the_leader_already_exited() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "trap '' HUP; sleep 600 & exit 0".to_string(),
        ];
        let producer = PtyProducer::spawn(&argv, 80, 24).unwrap();

        assert!(
            wait_for_zombie(producer.pid, Duration::from_secs(5)),
            "the leader never exited within 5s"
        );
        let descendant_pid = wait_for_descendant_by_pgrp(producer.pid, Duration::from_secs(5))
            .expect(
                "the backgrounded sleep must survive the leader's own exit (SIGHUP is ignored)",
            );

        drop(producer); // no terminate_domain/close_output_side call -- Drop alone must do this

        assert!(
            wait_until_dead(descendant_pid, Duration::from_secs(5)),
            "the surviving descendant was still alive 5s after Drop"
        );
    }

    /// The UNREAPED ZOMBIE LEADER is the deterministic
    /// case of the identical property this producer's own `domain_is_empty`
    /// must get right: a live leader means "not empty"; the SAME leader,
    /// killed and left an unreaped zombie (this producer's own contract
    /// -- see the module doc's third point), must read as "empty".
    #[test]
    #[cfg(target_os = "linux")]
    fn domain_is_empty_ignores_a_zombie_leader() {
        let producer =
            PtyProducer::spawn(&["sleep".to_string(), "600".to_string()], 80, 24).unwrap();

        assert!(
            !producer.domain_is_empty().unwrap(),
            "a live leader means the domain is not empty"
        );

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

/// The thread's plan for the PTY flag calls: which one fails, and the descriptors it saw.
#[cfg(test)]
mod flag_plan {
    use std::cell::RefCell;
    use std::os::fd::RawFd;

    #[derive(Default)]
    struct Plan {
        fail: Option<(&'static str, libc::c_int)>,
        seen: Vec<(&'static str, RawFd)>,
    }

    thread_local! {
        static PLAN: RefCell<Plan> = RefCell::new(Plan::default());
    }

    pub(super) fn arm(end: &'static str, cmd: libc::c_int) {
        PLAN.with(|plan| {
            *plan.borrow_mut() = Plan {
                fail: Some((end, cmd)),
                seen: Vec::new(),
            }
        });
    }

    pub(super) fn take_seen() -> Vec<(&'static str, RawFd)> {
        PLAN.with(|plan| std::mem::take(&mut *plan.borrow_mut()).seen)
    }

    pub(super) fn fails(end: &'static str, fd: RawFd, cmd: libc::c_int) -> bool {
        PLAN.with(|plan| {
            let mut plan = plan.borrow_mut();
            if !plan.seen.contains(&(end, fd)) {
                plan.seen.push((end, fd));
            }
            plan.fail == Some((end, cmd))
        })
    }
}

/// Both PTY ends get close-on-exec before publication, and either flag call failing is the factory's error with both
/// ends closed and no child started.
#[cfg(test)]
mod flag_tests {
    use super::{flag_plan, Producer, PtyProducer};
    use std::os::fd::AsRawFd;

    fn fd_is_closed(fd: std::os::fd::RawFd) -> bool {
        let rc = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
    }

    fn flag_failure(test: &str, end: &'static str, cmd: libc::c_int) {
        if !crate::test_isolated::run_isolated(test) {
            return;
        }
        flag_plan::arm(end, cmd);
        let argv = ["/bin/sh", "-c", "exit 0"].map(String::from);
        let spawned = PtyProducer::spawn(&argv, 80, 24);
        let seen = flag_plan::take_seen();
        assert!(
            spawned.is_err(),
            "PTY flag failure was not returned ({end} {cmd})"
        );
        assert_eq!(
            seen.len().min(2),
            seen.len(),
            "the factory flagged only its two ends: {seen:?}"
        );
        assert!(
            seen.iter().any(|(e, _)| *e == end),
            "the failing end was never flagged: {seen:?}"
        );
        for (e, fd) in seen {
            assert!(
                fd_is_closed(fd),
                "the {e} end (fd {fd}) was left open after the failure"
            );
        }
        let child = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(child, -1, "a child was started despite the failure");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        eprintln!("flag-failure-proof test={test} end={end} cmd={cmd} bodies=1");
    }

    #[test]
    fn master_getfd_failure_is_returned() {
        flag_failure(
            "capsule::producer::pty::flag_tests::master_getfd_failure_is_returned",
            "master",
            libc::F_GETFD,
        );
    }

    #[test]
    fn master_setfd_failure_is_returned() {
        flag_failure(
            "capsule::producer::pty::flag_tests::master_setfd_failure_is_returned",
            "master",
            libc::F_SETFD,
        );
    }

    #[test]
    fn slave_getfd_failure_is_returned() {
        flag_failure(
            "capsule::producer::pty::flag_tests::slave_getfd_failure_is_returned",
            "slave",
            libc::F_GETFD,
        );
    }

    #[test]
    fn slave_setfd_failure_is_returned() {
        flag_failure(
            "capsule::producer::pty::flag_tests::slave_setfd_failure_is_returned",
            "slave",
            libc::F_SETFD,
        );
    }

    /// Publication: a successfully spawned producer's master and held slave are close-on-exec.
    #[test]
    fn a_published_pty_pair_is_close_on_exec() {
        let producer =
            PtyProducer::spawn(&["sleep".to_string(), "600".to_string()], 80, 24).unwrap();
        for (end, fd) in [
            (
                "master",
                producer.reader_fd.as_ref().expect("the master").as_raw_fd(),
            ),
            (
                "slave",
                producer.slave.as_ref().expect("the held slave").as_raw_fd(),
            ),
        ] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(
                flags >= 0 && flags & libc::FD_CLOEXEC != 0,
                "the {end} end is not close-on-exec at publication"
            );
        }
    }
}
