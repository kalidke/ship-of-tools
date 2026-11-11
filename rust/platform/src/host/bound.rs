//! A process bounds itself: the OS mechanisms a program that reads untrusted input sets on itself before it reads a
//! byte (guarantee 18). Each ends the process by an exit code the parent can read, never a signal, a core file or a
//! crash dialog. The capped allocator is `host::heap_cap`. `exit_now` takes no lock and allocates nothing.

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

#[cfg(test)]
#[path = "bound_tests.rs"]
pub(crate) mod tests;
