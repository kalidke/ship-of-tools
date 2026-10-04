//! The platform subsystem: state dirs, durable publication, kernel locks.
//! The glob re-exports below are the module's interface: callers name
//! `sot_log::host::wide_null`, `sot_log::host::daemon_lock_path` and so on.

mod durable;
mod lock;
mod pinned_dir;
pub mod state_dir;
mod volume;
pub mod winhandle;
#[cfg(windows)]
mod winsec;

#[cfg(windows)]
use crate::Error;

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
    let budget = crate::lane::wire::MAX_SUPERVISOR_STRING_LEN.saturating_sub(4 + head.len() + code.len() + sep.len());
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
pub(crate) fn duration_to_wait_ms(timeout: std::time::Duration) -> u32 {
    u32::try_from(timeout.as_millis())
        .unwrap_or(u32::MAX)
        .min(u32::MAX - 1)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// The field defect this guards: a terminal-exit reason built from a
    /// deep voyage path (`state_dir/voyages/<uuid>/<segment>`) pushed the
    /// `(os error N)` clean off the end of the fixed-width record field
    /// once `bounded_detail` truncated it. The OS error code must survive
    /// even when the path is far longer than the whole budget, and the
    /// message must still name the op and end with the path's TAIL, not
    /// its head.
    #[test]
    #[cfg(windows)]
    fn io_ctx_keeps_the_os_error_code_and_the_path_tail_under_a_very_long_path() {
        let deep = std::path::PathBuf::from(format!(
            r"C:\Users\somebody\AppData\Local\sot\state\sot\workspaces\{}\voyages\{}\{}",
            "ws-".to_string() + &"x".repeat(60),
            "11111111-2222-3333-4444-555555555555",
            "segment-recognizable-tail.sotseg.tmp",
        ));
        let os_err = std::io::Error::from_raw_os_error(206); // ERROR_FILENAME_EXCED_RANGE
        let wrapped = io_ctx(os_err, format_args!("MoveFileExW {deep:?} -> {deep:?}"));
        let msg = format!("{wrapped}");
        assert!(msg.len() <= crate::lane::wire::MAX_SUPERVISOR_STRING_LEN, "{} bytes: {msg}", msg.len());
        assert!(msg.starts_with("io: MoveFileExW (os error 206): "), "{msg}");
        assert!(msg.ends_with("segment-recognizable-tail.sotseg.tmp\""), "{msg}");
    }

    #[test]
    #[cfg(windows)]
    fn io_ctx_passes_a_short_message_through_unchanged_but_for_the_code() {
        let short = std::path::PathBuf::from(r"C:\a\b.tmp");
        let os_err = std::io::Error::from_raw_os_error(5); // ERROR_ACCESS_DENIED
        let wrapped = io_ctx(os_err, format_args!("FlushFileBuffers dir {short:?}"));
        let msg = format!("{wrapped}");
        assert!(msg.starts_with("io: FlushFileBuffers dir (os error 5): "), "{msg}");
        assert!(msg.ends_with(r#"b.tmp""#), "{msg}");
        // Nothing lost: the code and the whole (short) path both survive.
        assert!(msg.contains("a") && msg.contains("b.tmp"), "{msg}");
    }

    /// No path at all in `what` (e.g. `OpenProcessToken`): the separator
    /// must not leave a dangling `": "` with nothing after it.
    #[test]
    #[cfg(windows)]
    fn io_ctx_with_no_path_in_what_has_no_dangling_separator() {
        let os_err = std::io::Error::from_raw_os_error(1008);
        let wrapped = io_ctx(os_err, format_args!("OpenProcessToken"));
        let msg = format!("{wrapped}");
        assert_eq!(msg, "io: OpenProcessToken (os error 1008)");
    }
}

pub use durable::*;
pub use lock::*;
pub use pinned_dir::*;
pub use volume::*;
#[cfg(windows)]
pub use winsec::*;
