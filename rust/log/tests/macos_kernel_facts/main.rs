#![cfg(target_os = "macos")]
//! macOS kernel-fact regression tests — the OS behaviours the macOS lane
//! is blocked on, pinned here so that a future OS change reports itself as
//! a named red test rather than as a silent auth or capsule regression
//! months later.
//!
//! The whole file is `#![cfg(target_os = "macos")]`: it compiles away to
//! nothing everywhere else, so the blocking `macos-latest` CI job
//! (`cargo test --workspace --locked`) is the only place any of them runs.
//!
//! # Fact 1 — does the kernel serve the SERVER's identity to a CLIENT?
//!
//! `src/identity/challenge_unix.rs` (Linux only, read its "Why `SO_PEERCRED` on the
//! CLIENT's own fd works" section) proves the peer's pid from the CLIENT's
//! OWN fd: `SO_PEERCRED` is latched at `connect(2)` onto BOTH ends of a
//! connected `AF_UNIX` socket, so a client reading its own socket learns
//! the real server pid — which is the entire foundation of
//! `authenticate_server()`. macOS has no `SO_PEERCRED`, and `getpeereid`
//! yields euid/egid only: no pid, and nothing that pins an identity
//! against pid reuse. The candidate replacement is
//! `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)`, whose `audit_token_t` carries
//! both a pid (word 5) and a pidversion (word 7 — the reuse generation,
//! macOS's own answer to the `/proc/<pid>/stat` start-time pin
//! `challenge_unix.rs` uses).
//!
//! THE DIRECTION IS THE WHOLE POINT. A token that only resolves
//! server-side (the server learns its client) is not enough:
//! `authenticate_server` needs the CLIENT to learn the SERVER. So this
//! test uses two REAL processes joined by a real `connect(2)` — a
//! `socketpair` would prove nothing (no `connect` ever happens) and a
//! server thread inside the test process would prove nothing (same pid).
//! It ASSERTS the client's half and merely RECORDS the server's half,
//! which costs one extra syscall and tells us whether the mechanism works
//! at all in the case where the interesting direction does not.
//!
//! # Fact 2 — does closing a pty master reap the child on the slave side?
//!
//! The Linux capsule leans on `PR_SET_PDEATHSIG` (`src/capsule/producer/pty/`),
//! which macOS does not have. If pty hangup does not reap a producer
//! there, the pipe lease planned for the next milestone stops being
//! belt-and-braces and becomes the only thing between a dead supervisor
//! and an orphaned producer.
//!
//! Asked TWICE, because the configuration matters: fact 2 drops the
//! parent's slave right after the spawn, and fact 2b holds it across the
//! master close — which is the shape the capsule actually ships (ADR 0043
//! decision 12 holds a slave for the whole run). Only fact 2b speaks for
//! the shipped configuration, and it is the gate before a macOS capsule
//! reaches a user: a child that survives there means a dead capsule can
//! orphan an agent forever.
//!
//! # Fact 7 — how much of a producer's final output does the revoke eat?
//!
//! A BSD session leader's exit revokes every descriptor on its controlling
//! terminal and flushes the tty's queues, so bytes written but not yet
//! drained at that instant are lost — named in decision 12's amendment as
//! the one thing a Mac user's record can lack. Fact 7 measures the size of
//! that window on a real kernel, in the shipped (slave-held)
//! configuration, and asserts no number: the quantity is the kernel's to
//! choose, and the honest thing is to print it.
//!
//! # Facts 3–6 — the kqueue death watch (`EVFILT_PROC`/`NOTE_EXIT`)
//!
//! Linux proves a peer is gone with a pidfd: it names the *instance*, it is
//! level-triggered (readable forever once the process exits), and an attach
//! to a pid with no process behind it simply fails. macOS has no single
//! object with all three properties. The candidate replacement is a kqueue
//! knote — `kevent(EV_ADD, EVFILT_PROC, NOTE_EXIT, ident = pid)` — which
//! attaches to a `proc`, not to a number, and is therefore the only macOS
//! primitive that can make an *un-fired* registration mean "this pid still
//! names the process I proved". (It has to be: there is no user-space API
//! that reads another process's `pidversion`, so `reverify` cannot be a
//! re-read of the identity the way it is on Linux.) Four kernel behaviours
//! carry that design, and nobody on this project can observe any of them:
//!
//! - **Fact 3 — once, and only once.** The watch treats "an exit was ever
//!   delivered" as proof, and asks the question with a non-blocking drain.
//!   Whether a spent knote re-delivers decides whether the design's exit
//!   latch is load-bearing or redundant.
//! - **Fact 4 — a zombie.** Attach to a process that has exited and has not
//!   been reaped: `ESRCH`, or an attach that fires at once? The design is
//!   safe under either and branches on the answer — but the branch must be
//!   chosen by a test, not by a comment.
//! - **Fact 5 — a reaped pid.** An attach to a pid whose process is fully
//!   gone must FAIL. This is the fail-closed path that makes the challenge's
//!   registration ordering safe.
//! - **Fact 6 — reuse.** The single most load-bearing assumption in the
//!   port: a knote is bound to a process, not to a pid number. Read that
//!   test's own comment for exactly what it proves and what it does not —
//!   forcing a real pid wrap is not something a CI test will do.
//!
//! # A red result here is a decision, not a bug to silence
//!
//! No test here asserts a preference; each pins what the kernel actually
//! does. A failure is an input the owner rules on (accept a weaker macOS
//! identity; promote the pipe lease to load-bearing) — never something to
//! relax until it goes green. Every assertion prints every value it
//! observed, because a CI log is the only channel these facts have.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Bound on every wait in this file. Generous (a cold CI runner spawning a
/// second copy of this test binary is not fast) but finite: nothing here
/// may hang the macOS job.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(20);
const CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a hung-up pty is given to kill the child before we call it a
/// survivor. Kernel signal delivery is immediate; this is slack, not a
/// measurement.
const PTY_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the pty child is given to exec and take the pty as its
/// controlling terminal (it announces itself on the tty — see the test).
const PTY_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Write one observed fact to the process's REAL stderr.
///
/// `println!`/`eprintln!` go through libtest's per-thread output capture,
/// which DISCARDS everything a PASSING test printed — and a passing test
/// is exactly the case where we still want the numbers (which signal
/// killed the child, how long the kernel took). A direct write to the
/// `Stderr` handle does not consult that capture, so these lines reach the
/// CI log either way. Failure messages need no such help: an assertion
/// message is always printed.
fn fact(line: &str) {
    let _ = writeln!(std::io::stderr(), "[macos-kernel-fact] {line}");
}

/// Reap `child` within `bound`, killing it if it overruns. Returns the
/// status text either way — this is diagnostics, not an assertion.
fn reap_bounded(child: &mut Child, bound: Duration) -> String {
    let deadline = Instant::now() + bound;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return format!("{status}"),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return format!("still running after {bound:?} -- killed");
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return format!("try_wait failed: {e}"),
        }
    }
}

mod kqueue;
mod peertoken;
mod pty_hangup;
mod revoke;
