//! A process bounds itself: the OS mechanisms a program that reads untrusted input sets on itself before it reads a
//! byte (guarantee 18). `exit_now` and the two Unix handlers (`exit_on_crash`, `limit_cpu`) end the process by an exit
//! code the parent can read; `end_with_parent` and `limit_cpu`'s hard limit end it by `SIGKILL`; on Windows
//! `exit_on_crash` only keeps a dialog away and the status is the OS's. `close_inherited` ends nothing; `take_stdout` gives the process's stdout to its caller alone. The capped
//! allocator is `host::heap_cap`. Nothing here allocates once it has returned, and `exit_now` and the signal handlers
//! use `_exit`, which runs no handler and takes no lock.

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
/// soft limit was lowered) stays open. It closes what the process's own libraries hold too, so it is a process's first
/// act, before any thread starts or any library opens a descriptor it keeps; on macOS the frameworks a binary links
/// have run their initializers before `main`, and closing a descriptor one of them guards is a crash.
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

/// Registers `exit_by_signal` for `signal`. `SA_ONSTACK` runs it on the thread's alternate stack when there is one, so
/// a signal that arrives on a thread whose own stack is nearly full still reaches `_exit`. (Rust's stack-overflow
/// report raises its `SIGABRT` from the alternate stack already, so the overflow test passes without the flag.)
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
/// crash status (`code` is unused), and whether a report is still written is not known here. A crash by another signal
/// (`SIGSEGV` outside a stack overflow, `SIGBUS`, `SIGILL`) is not covered.
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
/// that exits with `code`; the hard limit's `SIGKILL` follows if the handler cannot run. Counts all threads' time. A
/// zero is a one-second soft limit on Linux. Windows has no counterpart here: the parent's wall clock is its bound.
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

/// Takes this process's stdout for the caller alone: the returned file is a duplicate of descriptor 1 (Windows: of the
/// standard output handle) that no child inherits, and descriptor 1, the standard output handle and the C runtime's
/// descriptor 1 then lead to the null device, so a print, a `write(1, ..)` or a library's `printf` after this call
/// reaches nobody. A process whose stdout carries a protocol calls it before it reads, and writes only to the file.
/// On an error the caller writes nothing to stdout: on Unix stdout is unchanged and the copy closed; on Windows it may
/// already lead to the null device. What still reaches the old stdout: a copy of descriptor 1 or of the handle made
/// before this call, a Unix process forked without an exec (close-on-exec acts at exec), and on Windows a raw handle
/// value cached before the call (the runtime's `_dup2` closes the original handle). A console or terminal opened by
/// name is not stdout, whatever stdout is, and is not touched.
pub fn take_stdout() -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: fcntl on descriptor 1 returns a new descriptor this function owns, or -1.
        let private = unsafe { libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3) };
        if private < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `private` is open and owned by nothing else.
        let private = unsafe { std::fs::File::from_raw_fd(private) };
        let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
        // SAFETY: both descriptors are open; descriptor 1 is open (the duplicate above), so `null` is not it.
        match unsafe { libc::dup2(null.as_raw_fd(), 1) } {
            -1 => Err(io::Error::last_os_error()),
            _ => Ok(private),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawHandle, IntoRawHandle};
        use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_OUTPUT_HANDLE};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        extern "C" {
            fn _open_osfhandle(handle: isize, flags: i32) -> i32;
            fn _get_osfhandle(fd: i32) -> isize;
            fn _dup2(from: i32, to: i32) -> i32;
            fn _close(fd: i32) -> i32;
        }
        // SAFETY: the handle calls take values this block produced; the new file owns the duplicate it wraps.
        unsafe {
            let (me, out) = (GetCurrentProcess(), GetStdHandle(STD_OUTPUT_HANDLE));
            if out.is_null() || out == INVALID_HANDLE_VALUE {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no standard output handle"));
            }
            let mut copy: HANDLE = std::ptr::null_mut();
            if DuplicateHandle(me, out, me, &mut copy, 0, 0, DUPLICATE_SAME_ACCESS) == 0 {
                return Err(io::Error::last_os_error());
            }
            let private = std::fs::File::from_raw_handle(copy as _);
            let null = std::fs::OpenOptions::new().write(true).open("NUL")?;
            // The runtime's descriptor 1 takes the null device (its fd owns the handle until closed), then the
            // standard output handle follows it. In a console program `_dup2` already sets that handle; the explicit
            // call is for a GUI-subsystem caller, where the runtime leaves it naming the handle it just closed. Keep it.
            let fd = _open_osfhandle(null.into_raw_handle() as isize, 0);
            if fd < 0 || _dup2(fd, 1) != 0 {
                return Err(io::Error::other("the C runtime would not take the null device"));
            }
            _close(fd);
            if SetStdHandle(STD_OUTPUT_HANDLE, _get_osfhandle(1) as HANDLE) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(private)
        }
    }
}

#[cfg(test)]
#[path = "bound_tests.rs"]
pub(crate) mod tests;
