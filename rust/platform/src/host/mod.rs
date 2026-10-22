//! The platform subsystem: state dirs, durable publication, kernel locks, held folders, the redial pace.
//! The glob re-exports below are the module's interface: callers name
//! `sot_platform::host::wide_null`, `sot_platform::host::daemon_lock_path` and so on.

mod durable;
pub mod env;
#[cfg(unix)]
pub mod fd;
mod held_dir;
mod lock;
pub mod paths;
mod pinned_dir;
pub mod process_tree;
pub mod redial;
pub mod state_dir;
mod storage;
mod volume;
pub mod winhandle;
#[cfg(windows)]
mod winsec;

#[cfg(windows)]
use crate::Error;

/// The most bytes of an error's text a caller may carry in a fixed-width record field (the capsule supervisor's
/// terminal-exit reason is one, `lane::wire::MAX_SUPERVISOR_STRING_LEN`, which takes this value); `io_ctx` keeps its
/// text within it.
pub const ERROR_DETAIL_BOUND: usize = 128;

/// Bounded-retry deadline shared by every transient-absorbing loop here:
/// long enough to outlive an AV/indexer hold or a just-released kernel
/// lock, short enough that a persistent condition still fails loudly.
const RETRY_DEADLINE_MS: u64 = 250;
const RETRY_STEP_MS: u64 = 10;

/// Wrap an OS error with the failing op + path, preserving the `ErrorKind`
/// (callers match on it — the CAS race needs `AlreadyExists`). A bare
/// "Access is denied" from deep inside a publication sequence is
/// undiagnosable; a loud failure must say where.
///
/// The OS error code goes right after `what`'s operation name (its text
/// up to the first `"`, where every call site switches to a `{path:?}`)
/// and is NEVER dropped: the supervisor's terminal-exit reason is a
/// fixed-width record field (`wire::MAX_SUPERVISOR_STRING_LEN` bytes,
/// `supervisor::bounded_detail`) that truncates blindly from the tail — a
/// raw "MoveFileExW <two long paths>: <message> (os error N)" buried the
/// one number an operator can act on. Remaining budget goes to the
/// path's own TAIL (a filename says more than a repeated drive head).
#[cfg(windows)]
fn io_ctx(e: std::io::Error, what: std::fmt::Arguments<'_>) -> Error {
    if storage::native_storage_code(&e).is_some() {
        return Error::Io(e);
    }
    let code = match e.raw_os_error() {
        Some(c) => format!(" (os error {c})"),
        None => String::new(),
    };
    let detail = format!("{what}");
    let mut head_end = detail.find('"').unwrap_or(detail.len()).min(40);
    while head_end > 0 && !detail.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let (head, path) = detail.split_at(head_end);
    let head = head.trim_end();
    let sep = if path.is_empty() { "" } else { ": " };
    // "io: " (4 bytes) is `Error`'s own Display wrapper (`#[error("io:
    // {0}")]`) around whatever this returns.
    let budget = ERROR_DETAIL_BOUND.saturating_sub(4 + head.len() + code.len() + sep.len());
    let tail = if path.len() > budget {
        let keep = budget.saturating_sub(3); // room for the "..." marker itself
        let mut cut = path.len().saturating_sub(keep);
        while cut < path.len() && !path.is_char_boundary(cut) {
            cut += 1;
        }
        format!("...{}", &path[cut..])
    } else {
        path.to_string()
    };
    Error::Io(std::io::Error::new(e.kind(), format!("{head}{code}{sep}{tail}")))
}

/// A `Duration` clamped to a Win32 wait-call millisecond parameter,
/// NEVER `INFINITE` (`u32::MAX`, 0xFFFFFFFF) — a caller-supplied duration
/// long enough to round to that exact value would silently wait forever
/// instead of the bounded wait it asked for (ADR 0041 U0 round-1 finding
/// 8). Shared by every bounded Win32 process wait in this crate
/// (`conpty::PrimaryProcess::wait`, `challenge_win::wait_handle` — used by
/// both `ChallengedProcess` and `supervisor::probe::win::SpawnedChild`) so the guard
/// exists exactly once, not once per call site.
#[cfg(windows)]
pub fn duration_to_wait_ms(timeout: std::time::Duration) -> u32 {
    u32::try_from(timeout.as_millis())
        .unwrap_or(u32::MAX)
        .min(u32::MAX - 1)
}

#[cfg(all(test, windows))]
#[path = "tests.rs"]
mod tests;

pub use durable::*;
pub use held_dir::*;
#[cfg(unix)]
pub use fd::inherit_none;
pub use lock::*;
pub use pinned_dir::*;
pub use storage::native_storage_code;
pub use volume::*;
#[cfg(windows)]
pub use winsec::*;
