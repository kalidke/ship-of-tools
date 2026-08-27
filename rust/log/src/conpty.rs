//! Owned ConPTY + job containment (ADR 0041 "Containment and the owned
//! ConPTY layer"). Windows-only.
//!
//! As simple as possible but no simpler: simple and elegant leads to
//! performance and security. Low-level primitives ONLY — no capsule
//! runtime, no frame emission, no vt100, no query-response state machine,
//! no writer loop. That is the NEXT unit's job; this one hands it verbs
//! (`spawn`, `terminate`, `active_processes`, `resize`, `close_pty`, the
//! reader/writer pipe ends) and gets out of the way. One struct per OS
//! resource; a field exists only if it names the invariant it serves; RAII
//! drop order IS the exceptional-unwind path — `spawn`'s error branches
//! never manually clean up anything, they just return and let Rust's own
//! reverse-declaration-order drops do it.
//!
//! `portable-pty` is deliberately not used: its ConPTY path installs
//! exactly one process-thread attribute and exposes no job hook, so atomic
//! containment (assigning the job BEFORE the child's first instruction
//! runs) is impossible through it. This module owns the small Win32 spawn
//! sequence directly via `windows-sys`, at the same raw-API altitude the
//! Linux side uses for `pre_exec`.

#![cfg(windows)]

use crate::{Error, Result};
use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HANDLE};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, InitializeProcThreadAttributeList,
    UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTUPINFOEXW,
};

/// One spawn stage that failed, with the raw Win32/HRESULT error underneath
/// — folded into [`Error::Spawn`] so a caller can commit `producer_dead
/// {spawn_failed}` with a real diagnostic (which call, what error) instead
/// of a bare string. `stage` names match the actual API being called
/// (`"CreatePipe(in)"`, `"CreateProcessW"`, ...).
#[derive(Debug, thiserror::Error)]
#[error("stage {stage}: {source}")]
pub struct SpawnError {
    pub stage: &'static str,
    #[source]
    pub source: std::io::Error,
}

fn spawn_err(stage: &'static str, source: std::io::Error) -> Error {
    Error::Spawn(SpawnError { stage, source })
}

/// `CreatePseudoConsole`/`ResizePseudoConsole` report failure as
/// `HRESULT_FROM_WIN32(GetLastError())` (Microsoft's own sample code uses
/// exactly that macro) — decode it back to the real Win32 error so the
/// resulting `io::Error` gets std's accurate `ErrorKind` mapping instead of
/// an opaque HRESULT integer. Anything outside the Win32 facility (should
/// not happen for these two calls) is reported as the raw hex value.
fn hresult_to_io_error(hr: i32) -> std::io::Error {
    const FACILITY_WIN32: u32 = 0x8007_0000;
    let bits = hr as u32;
    if bits & 0xFFFF_0000 == FACILITY_WIN32 {
        std::io::Error::from_raw_os_error((bits & 0xFFFF) as i32)
    } else {
        std::io::Error::other(format!("HRESULT {hr:#010x}"))
    }
}

/// NUL-terminated UTF-16 for an arbitrary Rust string. Distinct from a
/// path-normalizing helper on purpose: a command line is not a path and
/// must not get `\\?\`-prefixed or separator-normalized.
fn wide_null(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Windows has no execv: `CreateProcessW` takes ONE command-line string,
/// and the child's C runtime re-splits it with the standard MSVCRT rule
/// (well-documented: an even run of backslashes before a `"` is halved and
/// the quote is a delimiter; an odd run is halved-and-one-more, and that
/// remaining backslash escapes the quote into a literal). This function is
/// the inverse: verbatim if the argument needs no protection (non-empty,
/// no space/tab/quote); otherwise wrapped in `"..."` with backslashes
/// doubled immediately before a literal quote or the closing quote, so the
/// receiving parser reconstructs exactly the original bytes.
fn quote_arg(arg: &str, out: &mut String) {
    let needs_quotes = arg.is_empty() || arg.contains([' ', '\t', '"']);
    if !needs_quotes {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes: u32 = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // Trailing backslashes precede the closing quote we're about to add —
    // double them so they don't escape it instead.
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
}

/// Build one `CreateProcessW` command-line string from `argv`, `argv[0]`
/// included (there is no separate `lpApplicationName` here — `argv[0]` is
/// resolved by the same PATH search `CreateProcessW` performs for any
/// unqualified name, matching Microsoft's own "Creating the Hosted Process"
/// walkthrough, which passes `NULL` for `lpApplicationName` and puts the
/// executable as the first token of the command line).
fn build_command_line(argv: &[String]) -> String {
    let mut out = String::new();
    for (i, arg) in argv.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        quote_arg(arg, &mut out);
    }
    out
}

/// One end of an anonymous pipe, non-inheritable by construction: per
/// `CreatePipe`'s own docs, a `NULL` `lpPipeAttributes` means "the handle
/// cannot be inherited" — no explicit `SECURITY_ATTRIBUTES.bInheritHandle`
/// needed. Wrapped as `std::fs::File` rather than a bare handle: the pipe
/// is already synchronous (exactly what ConPTY requires), so `File`'s
/// blocking `Read`/`Write` map onto it with zero translation layer, RAII
/// closes it on drop, and the next unit's reader/writer threads get
/// `Read`/`Write` for free instead of hand-rolled `ReadFile`/`WriteFile`
/// wrappers.
fn create_pipe_pair() -> std::io::Result<(File, File)> {
    let mut read_handle: HANDLE = std::ptr::null_mut();
    let mut write_handle: HANDLE = std::ptr::null_mut();
    if unsafe { CreatePipe(&mut read_handle, &mut write_handle, std::ptr::null(), 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let read = unsafe { File::from_raw_handle(read_handle as RawHandle) };
    let write = unsafe { File::from_raw_handle(write_handle as RawHandle) };
    Ok((read, write))
}

/// Whether THIS (spawning) process is itself already running inside some
/// job, via `IsProcessInJob(self, NULL)`. Observation only — never a claim
/// of containment authority (the #119 locator-must-declare rule: absence of
/// a `kill_domain` fact is not itself a claim, and nothing here is named
/// one). `None` means the probe itself failed; that failure must not abort
/// a spawn that could otherwise succeed, so this is infallible to the
/// caller.
fn is_current_process_in_a_job() -> Option<bool> {
    let mut result: BOOL = 0;
    let ok = unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut result) };
    if ok == 0 {
        return None;
    }
    Some(result != 0)
}

/// Non-authoritative spawn diagnostics (ADR 0041 / #119 locator-must-declare:
/// observation, never authority — nothing here is named `kill_domain`). A
/// capsule folds this into its own spawn-detail record.
#[derive(Debug, Clone, Copy)]
pub struct SpawnDetail {
    /// Whether the SPAWNING process was itself observed inside a job at the
    /// moment of spawn (`None` if the probe failed). A jobbed launcher
    /// (e.g. an SSH session) may deny job-list breakaway, weakening the new
    /// job's containment — detecting that denial and deciding the DEGRADED
    /// handoff is a later unit's job (ADR 0041: step 6 detects, step 4
    /// records what it's given); this field is only the raw observation
    /// such detection would need.
    pub spawning_process_was_jobbed: Option<bool>,
}

/// The anonymous containment job (ADR 0041): `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`,
/// no breakaway flags, never named. The job handle IS the lease — sole
/// handle, kept alive for as long as containment is wanted; the kernel
/// kills every in-job process when the LAST handle closes, however this
/// process dies (a hard crash included, since handle closure on process
/// exit is a kernel guarantee, not something our own code has to run).
/// "Reaps the tree" is scoped to in-job descendants — broker-mediated
/// spawning (WMI, COM activation, schtasks, services) is outside the
/// domain, the exact analog of the Linux external-supervisor carve-out.
pub struct AnonymousJob(OwnedHandle);

impl AnonymousJob {
    fn create() -> Result<Self> {
        // NULL name + NULL security attributes: anonymous, non-inheritable
        // by construction (same default as `CreatePipe`'s NULL arm).
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(spawn_err("CreateJobObjectW", std::io::Error::last_os_error()));
        }
        let owned = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };

        // Zero-initialized is a valid `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`
        // — every field is a plain integer (no pointers), so an all-zero
        // bit pattern is a well-defined value everywhere in the struct;
        // only `LimitFlags` needs a real value.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                owned.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(spawn_err("SetInformationJobObject", std::io::Error::last_os_error()));
        }
        Ok(Self(owned))
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }

    /// `TerminateJobObject`: the writer loop's termination sequence calls
    /// this BEFORE polling `active_processes` down to zero and only then
    /// closing the pseudoconsole (ADR 0041's pinned order) — this method is
    /// the raw call only, sequencing is the caller's.
    pub fn terminate(&self) -> Result<()> {
        if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
            return Err(spawn_err("TerminateJobObject", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// `QueryInformationJobObject(JobObjectBasicAccountingInformation)`,
    /// the `ActiveProcesses` field — NOT `TotalProcesses`, which is a
    /// cumulative ever-associated count that never decreases and would
    /// never observe "the tree is gone".
    pub fn active_processes(&self) -> Result<u32> {
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            QueryInformationJobObject(
                self.raw(),
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut c_void,
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(spawn_err("QueryInformationJobObject", std::io::Error::last_os_error()));
        }
        Ok(info.ActiveProcesses)
    }
}

/// The owned pseudoconsole (ADR 0041). Created with `dwFlags = 0` — no
/// `PSEUDOCONSOLE_INHERIT_CURSOR` — pinned by the step-4 spec gate: the
/// supervisor context this spawns from has no parent console worth
/// inheriting a cursor position from, and `INHERIT_CURSOR` additionally
/// obligates the caller to answer an asynchronous cursor-position query on
/// `hInput`/`hOutput`, which a plain owned-primitives layer has no writer
/// loop to do yet.
pub struct Pseudoconsole(Option<HPCON>);

impl Pseudoconsole {
    fn create(cols: u16, rows: u16, pty_in_read: HANDLE, pty_out_write: HANDLE) -> Result<Self> {
        let size = COORD { X: cols as i16, Y: rows as i16 };
        let mut hpc: HPCON = 0;
        let hr = unsafe { CreatePseudoConsole(size, pty_in_read, pty_out_write, 0, &mut hpc) };
        if hr < 0 {
            return Err(spawn_err("CreatePseudoConsole", hresult_to_io_error(hr)));
        }
        Ok(Self(Some(hpc)))
    }

    fn raw(&self) -> HPCON {
        self.0.expect("Pseudoconsole used after close_pty")
    }

    /// `ResizePseudoConsole`. Geometry validation (the 2x2..512x256 budget)
    /// is the CALLER's — this is the raw HRESULT-checked call.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let size = COORD { X: cols as i16, Y: rows as i16 };
        let hr = unsafe { ResizePseudoConsole(self.raw(), size) };
        if hr < 0 {
            return Err(spawn_err("ResizePseudoConsole", hresult_to_io_error(hr)));
        }
        Ok(())
    }

    /// `ClosePseudoConsole` — void, so this cannot itself fail, but it CAN
    /// BLOCK: pre-24H2 Windows documents that closing a pseudoconsole may
    /// emit a final frame to `hOutput` and will not return until that
    /// output (and, if `PSEUDOCONSOLE_INHERIT_CURSOR` were in use, a cursor
    /// query reply) is drained. Deliberately SEPARATE from reader shutdown
    /// and NOT joined with anything here: the correct call pattern is
    /// "reader thread already draining `hOutput` concurrently, THEN call
    /// this" — the reader drains THROUGH the close to EOF and is joined
    /// only AFTER EOF (ADR 0041, pinned: join-before-close is an
    /// implementation error). Consuming `self` is the type-level nudge
    /// against calling it twice; it is not, and cannot be, a type-level
    /// guarantee that a reader is actually running — that guarantee is the
    /// writer loop's, by construction of its own sequence.
    pub fn close_pty(mut self) {
        if let Some(hpc) = self.0.take() {
            unsafe { ClosePseudoConsole(hpc) };
        }
    }
}

impl Drop for Pseudoconsole {
    fn drop(&mut self) {
        // Reached only if `close_pty` was never called — a bug, or a
        // partial-spawn unwind where no reader thread ever existed to
        // drain anything (nothing was ever written to `hOutput` for a
        // pseudoconsole whose child process never got created). Spawning
        // a throwaway thread to run the close is the safety-net choice: it
        // can never block THIS thread (so a panic unwind or an early
        // return can never hang on it), at the cost of a leaked OS thread
        // in the pathological case where the close genuinely never
        // returns. The documented, optimized path for normal operation is
        // `close_pty` called directly by a caller whose reader is already
        // running concurrently.
        if let Some(hpc) = self.0.take() {
            std::thread::spawn(move || unsafe { ClosePseudoConsole(hpc) });
        }
    }
}

/// `STARTUPINFOEXW`'s attribute list, with EXACTLY two attributes:
/// `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` and `PROC_THREAD_ATTRIBUTE_JOB_LIST`.
/// The job-list assignment happens BEFORE the new process's initial thread
/// is allowed to run (documented Windows 10+ behavior) — that atomicity is
/// the entire point of this design over a post-`CreateProcess`
/// `AssignProcessToJobObject`, which leaves a window where the child could
/// already be running unfenced.
struct AttributeList {
    // `u64`-backed rather than `Vec<u8>`: the list is opaque, but it stores
    // pointer-sized `lpValue` references internally, and a `Vec<u8>` only
    // guarantees 1-byte alignment. Same defensive pattern as the DACL
    // unit's `TOKEN_USER` buffer, applied here because the OS does not
    // document this structure's actual alignment requirement either.
    buf: Vec<u64>,
    // BOXED, and the box is load-bearing: `UpdateProcThreadAttribute`
    // STORES the `lpValue` pointer (it does not copy the pointed-at array),
    // and `CreateProcessW` reads through it later — so the array must sit
    // at an address that survives this struct being returned BY VALUE from
    // `new`. An inline field's stack address dies at that move; a heap
    // address does not. This was found the honest way: the first real
    // Windows run failed every spawn with ERROR_INVALID_HANDLE.
    job_handles: Box<[HANDLE; 1]>,
}

impl AttributeList {
    fn new(hpc: HPCON, job: HANDLE) -> Result<Self> {
        // `hpc` is consumed by value into the attribute slot below; only
        // the job-handle ARRAY needs owned, move-stable storage.
        const ATTRIBUTE_COUNT: u32 = 2;
        let mut size: usize = 0;
        // Sizing call: always "fails" by design (NULL list) — only `size`
        // matters, per `InitializeProcThreadAttributeList`'s documented
        // double-call idiom.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), ATTRIBUTE_COUNT, 0, &mut size) };
        let words = size.div_ceil(8);
        let mut buf: Vec<u64> = vec![0u64; words];
        let list_ptr = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if unsafe { InitializeProcThreadAttributeList(list_ptr, ATTRIBUTE_COUNT, 0, &mut size) } == 0 {
            return Err(spawn_err(
                "InitializeProcThreadAttributeList",
                std::io::Error::last_os_error(),
            ));
        }

        let mut me = Self { buf, job_handles: Box::new([job]) };
        // Recomputed from `me.buf` (not the pre-move `buf`): moving a `Vec`
        // never relocates its heap allocation, but re-deriving the pointer
        // from its final resting place is one less thing to have to trust.
        let list_ptr = me.buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;

        // THE TWO ATTRIBUTES USE OPPOSITE CONVENTIONS — the trap that
        // failed every spawn on the first real Windows run:
        // PSEUDOCONSOLE passes the HPCON *value itself* as `lpValue`
        // (Microsoft's own walkthrough sample passes `hPC`, not `&hPC` —
        // the handle IS the pointer-sized payload); JOB_LIST passes a
        // *pointer to* a handle array. Passing `&hpc` here made Windows
        // treat a stack address as a console handle: ERROR_INVALID_HANDLE
        // from CreateProcessW, instantly, on every image.
        let pseudoconsole_ok = unsafe {
            UpdateProcThreadAttribute(
                list_ptr,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const c_void,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if pseudoconsole_ok == 0 {
            // No manual delete: `me` is already constructed, so its Drop
            // runs `DeleteProcThreadAttributeList` on this return — a
            // manual call here would DOUBLE-delete (Microsoft: exactly one
            // delete per initialized list), and would also violate this
            // module's own rule that error branches never clean up.
            return Err(spawn_err(
                "UpdateProcThreadAttribute(pseudoconsole)",
                std::io::Error::last_os_error(),
            ));
        }

        let job_list_ok = unsafe {
            UpdateProcThreadAttribute(
                list_ptr,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                me.job_handles.as_ptr() as *const c_void,
                std::mem::size_of_val(&me.job_handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if job_list_ok == 0 {
            // Same as above: `me`'s Drop owns the one delete.
            return Err(spawn_err(
                "UpdateProcThreadAttribute(job_list)",
                std::io::Error::last_os_error(),
            ));
        }

        Ok(me)
    }

    fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        let list_ptr = self.buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        unsafe { DeleteProcThreadAttributeList(list_ptr) };
    }
}

/// The result of one owned spawn: the containment job, the pseudoconsole,
/// the pipe ends this process communicates through, and non-authoritative
/// diagnostics. Deliberately no `Drop` impl of its own — that would be
/// ORCHESTRATION (deciding a teardown SEQUENCE), which is explicitly the
/// next unit's writer loop's job, not this primitives layer's. Dropping a
/// `ConptySpawn` without calling `job.terminate()` / `pty.close_pty()`
/// explicitly still safely releases every OS resource (each field's own
/// Drop — `OwnedHandle`'s `CloseHandle`, `Pseudoconsole`'s throwaway-thread
/// fallback — sees to that) but NOT in the ADR's pinned order; production
/// teardown must call the primitives explicitly.
pub struct ConptySpawn {
    pub job: AnonymousJob,
    pub pty: Pseudoconsole,
    /// Read the child's output (the pty's `hOutput`, our end of that pipe).
    pub reader: File,
    /// Write input to the child (the pty's `hInput`, our end of that pipe).
    pub writer: File,
    pub pid: u32,
    pub detail: SpawnDetail,
}

impl ConptySpawn {
    /// Spawn `argv[0]` with `argv[1..]` as arguments inside an owned
    /// pseudoconsole of `cols` x `rows`, contained by a fresh anonymous job.
    ///
    /// Stage order, pinned (ADR 0041): two pipe pairs → `CreatePseudoConsole`
    /// (flags 0) → the anonymous job FIRST (`CreateJobObjectW` +
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, no breakaway) → the two-attribute
    /// list (pseudoconsole + job list) → `CreateProcessW`. Every acquisition
    /// is a local RAII value; a failure at stage N returns immediately and
    /// lets Rust's own reverse-declaration-order drops unwind stages
    /// N-1..1 — no manual cleanup appears in any error branch below.
    pub fn spawn(argv: &[String], cols: u16, rows: u16) -> Result<Self> {
        if argv.is_empty() {
            return Err(Error::State("conpty spawn: empty argv".into()));
        }
        let spawning_process_was_jobbed = is_current_process_in_a_job();

        let (pty_in_read, writer) = create_pipe_pair().map_err(|e| spawn_err("CreatePipe(in)", e))?;
        let (reader, pty_out_write) = create_pipe_pair().map_err(|e| spawn_err("CreatePipe(out)", e))?;

        let pty = Pseudoconsole::create(
            cols,
            rows,
            pty_in_read.as_raw_handle() as HANDLE,
            pty_out_write.as_raw_handle() as HANDLE,
        )?;

        let job = AnonymousJob::create()?;

        let mut attrs = AttributeList::new(pty.raw(), job.raw())?;

        let cmdline = build_command_line(argv);
        let mut cmdline_wide = wide_null(&cmdline);

        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = attrs.as_ptr();

        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(), // lpApplicationName: NULL — argv[0] is the first command-line token (see build_command_line)
                cmdline_wide.as_mut_ptr(),
                std::ptr::null(), // lpProcessAttributes
                std::ptr::null(), // lpThreadAttributes
                0,                // bInheritHandles: FALSE — the pseudoconsole and job pass via the
                // attribute list, not handle inheritance (matches Microsoft's own
                // walkthrough sample, which also passes FALSE here)
                EXTENDED_STARTUPINFO_PRESENT,
                std::ptr::null(), // lpEnvironment: inherit ours
                std::ptr::null(), // lpCurrentDirectory: inherit ours
                &startup.StartupInfo,
                &mut pi,
            )
        };
        if ok == 0 {
            return Err(spawn_err("CreateProcessW", std::io::Error::last_os_error()));
        }

        // Success: free the attribute list now (its job is done) and close
        // OUR copies of the ConPTY-side pipe ends, per Microsoft's own
        // walkthrough ("Upon completion of the CreateProcess call... the
        // handles given during creation should be freed from this
        // process") — this is what lets EOF propagate to `reader` when the
        // pseudoconsole session ends, instead of our own dangling
        // reference keeping the pipe artificially alive.
        drop(attrs);
        drop(pty_in_read);
        drop(pty_out_write);
        unsafe { CloseHandle(pi.hThread) };
        // No `hProcess` field is kept: every primitive this layer exposes
        // (terminate, active_processes) goes through the JOB, not a
        // process handle, so holding one open here would be a resource
        // this module never uses — a caller that needs one can `OpenProcess`
        // by `pid` itself.
        unsafe { CloseHandle(pi.hProcess) };

        Ok(Self {
            job,
            pty,
            reader,
            writer,
            pid: pi.dwProcessId,
            detail: SpawnDetail { spawning_process_was_jobbed },
        })
    }
}

