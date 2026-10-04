//! Defect fix (field-proven, Windows): `CreateProcessW`'s `bInheritHandles`
//! (`Command`'s default `TRUE`) inherits EVERY `HANDLE_FLAG_INHERIT` handle
//! in the caller, not only the three wired into a child's `STARTUPINFO`. If
//! the daemon's OWN stdout/stderr (as its launcher opened them against the
//! fixed log path) are inheritable, they ride into every supervisor it
//! spawns and from there into every leg (`supervisor.rs`'s leg spawn
//! inherits its own current stdio by design, for its shared stderr —
//! decision 25) — a supervisor/leg outliving the daemon then keeps that
//! handle open indefinitely, so a replacement daemon's launcher can fail to
//! reopen the same log path. Fix: clear the flag on a process's own
//! inherited `STD_*_HANDLE`s before it spawns anything (never on a handle
//! made fresh for one child — those are already scoped). Only affects
//! future inheritance, not this process's own use of the handle.

#![cfg(windows)]

use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

fn clear_inherit(std_handle: STD_HANDLE) -> std::io::Result<()> {
    // SAFETY: plain FFI; the handle is only compared/passed back to the OS.
    unsafe {
        let h = GetStdHandle(std_handle);
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            return Ok(()); // nothing at this slot
        }
        if SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Hardens this process's own inherited stdin/stdout, plus stderr when
/// `also_stderr` (the daemon: `true`, it hands every spawn fresh `Stdio`;
/// the supervisor: `false` -- its stderr IS the shared log handle, decision
/// 25, that its leg spawn means to inherit).
pub fn harden_own_stdio(also_stderr: bool) -> std::io::Result<()> {
    clear_inherit(STD_INPUT_HANDLE)?;
    clear_inherit(STD_OUTPUT_HANDLE)?;
    if also_stderr {
        clear_inherit(STD_ERROR_HANDLE)?;
    }
    Ok(())
}
