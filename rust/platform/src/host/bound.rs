//! A process bounds itself: the OS mechanisms a program that reads untrusted input sets on itself before it reads a
//! byte (guarantee 18). Each ends the process by an exit code the parent can read, never a signal, a core file or a
//! crash dialog. The capped allocator is `host::heap_cap`. Nothing here allocates once it has returned, and `exit_now`
//! and the signal handlers use `_exit`, which runs no handler and takes no lock.

use std::io;

/// Ends this process at once with `code`, running no destructor, handler or `atexit` function, and allocating nothing
/// (`_exit`; Windows `TerminateProcess` on the process itself).
pub fn exit_now(code: i32) -> ! {
    #[cfg(unix)]
    // SAFETY: `_exit` takes no pointer and returns never.
    unsafe {
        libc::_exit(code)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        // SAFETY: the pseudo-handle of this process is always valid.
        unsafe { TerminateProcess(GetCurrentProcess(), code as u32) };
        std::process::abort()
    }
}

/// Closes every descriptor above stderr (Unix). Linux: `close_range(3, ~0)`, reaching every number; elsewhere, and on
/// a kernel that refuses the call, a loop to `getdtablesize()`, so a descriptor above that limit (one opened before the
/// soft limit was lowered) stays open.
#[cfg(unix)]
pub fn close_inherited() {
    // SAFETY: close_range and close act on this process's own descriptor table; a number not open is EBADF.
    #[cfg(target_os = "linux")]
    if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } == 0 {
        return;
    }
    close_to_table_size();
}

#[cfg(unix)]
fn close_to_table_size() {
    // SAFETY: as `close_inherited`.
    for fd in 3..unsafe { libc::getdtablesize() } {
        unsafe { libc::close(fd) };
    }
}

/// The code each signal's handler exits with.
#[cfg(unix)]
static CRASH_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
#[cfg(unix)]
static CPU_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn exit_by_signal(signal: libc::c_int) {
    let code = if signal == libc::SIGXCPU { &CPU_CODE } else { &CRASH_CODE };
    exit_now(code.load(std::sync::atomic::Ordering::Relaxed))
}

/// Registers `exit_by_signal` for `signal` on the alternate stack: Rust's stack-overflow handler aborts from there.
#[cfg(unix)]
fn exit_on_signal(signal: libc::c_int) -> io::Result<()> {
    // SAFETY: a zeroed `sigaction` is valid; the handler is async-signal-safe (an atomic load and `_exit`).
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = exit_by_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_ONSTACK;
        libc::sigemptyset(&mut action.sa_mask);
        match libc::sigaction(signal, &action, std::ptr::null_mut()) {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

/// A crash ends the process as an exit. Unix: `SIGABRT` (an abort, a panic abort, Rust's stack-overflow report) exits
/// with `code`, so there is no signal death, no core and no crash report. Windows: `SetErrorMode` with
/// `SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX`, so no error dialog stops the end; the exit status is the OS's own
/// (`code` is unused). A crash by another signal (`SIGSEGV` outside a stack overflow) is not covered.
pub fn exit_on_crash(code: i32) -> io::Result<()> {
    #[cfg(unix)]
    {
        CRASH_CODE.store(code, std::sync::atomic::Ordering::Relaxed);
        exit_on_signal(libc::SIGABRT)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
        };
        let _ = code;
        // SAFETY: a plain flag set on this process.
        unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
        Ok(())
    }
}

/// The process's CPU time is bounded (Unix): `RLIMIT_CPU` soft `seconds`, hard `seconds + 1`, and a `SIGXCPU` handler
/// that exits with `code`; the hard limit's `SIGKILL` follows if the handler cannot run. Counts all threads' time.
/// Windows has no counterpart here: the parent's wall clock is its bound.
#[cfg(unix)]
pub fn limit_cpu(seconds: u64, code: i32) -> io::Result<()> {
    CPU_CODE.store(code, std::sync::atomic::Ordering::Relaxed);
    exit_on_signal(libc::SIGXCPU)?;
    let limit = libc::rlimit { rlim_cur: seconds as libc::rlim_t, rlim_max: seconds.saturating_add(1) as libc::rlim_t };
    // SAFETY: a plain setrlimit on this process.
    match unsafe { libc::setrlimit(libc::RLIMIT_CPU, &limit) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

/// The process ends with its parent (Linux: `PR_SET_PDEATHSIG` with `SIGKILL`). It fires when the thread that started
/// this process ends, which is the whole parent only if that thread outlives its work, and a parent already gone before
/// this call is not noticed. Elsewhere nothing is done: macOS and Windows have no such call, and the caller's other
/// bounds (the CPU limit, a job) end an orphan.
pub fn end_with_parent() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    // SAFETY: a plain prctl on this process.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
#[path = "bound_tests.rs"]
pub(crate) mod tests;
