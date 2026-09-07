//! The `Producer` trait: the capsule writer loop's own nine call sites
//! into whatever OS primitive is actually running the child — ConPTY +
//! job containment on Windows (`producer_conpty.rs`), a bare PTY +
//! process group on Unix (`producer_pty.rs`, LU2b). Parameterizing
//! `capsule::run` over this trait is what lets ONE writer loop serve
//! every platform (ADR 0043 "Decisions for LU2", decision 11): the
//! frame factory, the output budget, the input WAL, the run-end marker,
//! the `AttachProto` service path, `ShutdownGuard`, rotation and sealing
//! are all platform-neutral and never touch this trait at all — only the
//! nine verbs below do, each copied verbatim from `capsule.rs`'s own
//! former (pre-LU2a) call sites into the ConPTY producer, nothing
//! invented. No `kind()` method: `producer_kind` stays a plain config
//! string the caller sets, never derived from this trait.
//!
//! **The EOF contract (decision 12) is universal, with no knob.** The
//! output side's own OS behavior differs — ConPTY keeps its output
//! handle open regardless of the child's lifetime until explicitly
//! closed; a Unix pty master returns EIO/EOF precisely when the child
//! dies — but the CONTRACT this trait's implementations must uphold is
//! identical on every platform: `Self::Output` reports EOF (or an I/O
//! error) ONLY after [`Producer::close_output_side`] has run. A pre-close
//! EOF/error is capsule-fatal everywhere (`capsule::run` bails unsealed,
//! ADR 0039's crash shape) — an implementation whose OS reports the end
//! earlier (a Unix producer) must hold it (flag + condvar) until
//! `close_output_side` releases it, rather than exposing a
//! platform-specific "is early EOF an anomaly here" flag that would serve
//! no invariant.

use crate::Result;
use std::io::{Read, Write};
use std::time::Duration;

/// A producer's own raw exit status (ADR 0043 decision 13). Windows
/// always yields `Code` and keeps the raw DWORD unsigned end-to-end (a
/// high-bit NTSTATUS is never sign-flipped — see
/// `exit_status_after_confirmed_exit`'s own doc); Unix yields `Code` for
/// a normal exit and `Signal` for a signal death, which has no code. In
/// the durable record, `producer_dead.detail` carries `exit_code` (u32)
/// for `Code` and `signal` (i32) for `Signal` — an ADDITIVE field, never
/// a schema change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Code(u32),
    Signal(i32),
}

/// The parent-death lease's own per-platform shape (ADR 0043 decision
/// 15): a named, kernel-brokered mutex on Windows (`crate::lease`),
/// checked by name; an inherited pipe read-end file descriptor on Unix,
/// checked by `producer_pty::parent_lease_fd_broken` — a real descendant
/// is the only thing that can hold the fd. `capsule::CapsuleConfig`'s
/// `parent_lease` field is `Option<ParentLease>` — `None` is this
/// crate's own manual-testing harness and every capsule test, unchanged
/// from before this type existed.
#[derive(Debug, Clone)]
pub enum ParentLease {
    #[cfg(windows)]
    NamedMutex(String),
    #[cfg(unix)]
    InheritedFd(std::os::fd::RawFd),
}

/// One producer under the capsule's writer loop — exactly the nine verbs
/// `capsule::run` calls against it, nothing invented (decision 11):
/// SPAWN, take the OUTPUT stream once, an INPUT sink, RESIZE, a bounded
/// WAIT, the raw EXIT STATUS (preconditioned on a confirmed exit), a
/// domain TERMINATE, a domain-EMPTY poll, and the teardown-time output
/// close.
pub trait Producer: Send + Sized {
    /// The producer's own output stream, read by the loop's dedicated
    /// reader thread — never this trait itself.
    type Output: Read + Send + 'static;

    /// Diagnostics merged into `producer_spawn.detail` BEFORE [`spawn`]
    /// is even attempted, so a spawn failure still records them —
    /// Windows contributes `spawning_process_was_jobbed`; Unix
    /// contributes nothing (`{}`). Always a JSON object.
    ///
    /// [`spawn`]: Producer::spawn
    fn pre_spawn_detail() -> serde_json::Value;

    /// Spawn `argv[0]` with `argv[1..]` as arguments, at `cols`x`rows`.
    /// Geometry is validated by the loop's own 2x2..512x256 budget
    /// BEFORE this is ever called — an implementation need not re-check
    /// it.
    fn spawn(argv: &[String], cols: u16, rows: u16) -> Result<Self>;

    /// Takes the producer's output stream. Called EXACTLY ONCE, before
    /// the loop's reader thread starts — never again after.
    fn take_output(&mut self) -> Self::Output;

    /// The producer's input sink — the WAL's own forward syscall
    /// (`run_input_wal`), and (Windows) the host-handshake reply write,
    /// both go through this.
    fn input(&mut self) -> &mut dyn Write;

    /// Resize the producer's terminal. Geometry is validated by the SAME
    /// budget a resize wire request is, by the caller — never this
    /// method.
    fn resize(&self, cols: u16, rows: u16) -> Result<()>;

    /// Non-blocking (`Duration::ZERO`) or bounded poll for the producer
    /// having exited. `Ok(true)`: exited within `timeout`. `Ok(false)`:
    /// timed out, still running.
    fn wait(&self, timeout: Duration) -> Result<bool>;

    /// The producer's raw exit status. PRECONDITION this method does not
    /// itself check: the caller has already independently confirmed the
    /// producer is no longer running (`wait` returning `true`, or
    /// `domain_is_empty` returning `Ok(true)`) before calling this.
    fn exit_status_after_confirmed_exit(&self) -> Result<ExitStatus>;

    /// Terminate the producer's whole containment domain (Windows: the
    /// job object; Unix: the process group) — idempotent: terminating an
    /// already-empty domain is a harmless no-op.
    fn terminate_domain(&self) -> Result<()>;

    /// Whether the containment domain is empty — every process in it
    /// reaped.
    fn domain_is_empty(&self) -> Result<bool>;

    /// Closes the producer's output side — teardown Phase B. This is the
    /// ONE call in this trait that may itself block the underlying OS
    /// (pre-24H2 Windows documents `ClosePseudoConsole` as capable of
    /// waiting indefinitely with nothing draining the output side), so an
    /// implementation runs the actual close on its OWN dedicated thread
    /// and returns that thread's `JoinHandle` immediately, never blocking
    /// THIS call itself — the loop keeps servicing `input()`/mgmt/output
    /// concurrently with the close in progress, exactly as the documented
    /// call pattern requires ("reader already draining, THEN call this").
    ///
    /// `&mut self`, not consumed: the loop still needs `wait`/
    /// `exit_status_after_confirmed_exit` on this SAME producer once the
    /// returned handle joins (on Windows, the process handle outlives the
    /// pseudoconsole) — an implementation holds its own output-side
    /// handle in an `Option`, taken here exactly once (a second call is a
    /// caller bug).
    fn close_output_side(&mut self) -> std::thread::JoinHandle<()>;
}
