//! The anonymous kill-on-close job (Windows): the one primitive containment's per-child tree and the capsule's
//! ConPTY leg both stand on. `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` alone, never named, and no breakaway permitted:
//! everything started inside dies with the run (ADR 0050 ruling (g)). The job handle IS the lease: sole handle, kept
//! alive as long as containment is wanted; the kernel kills every in-job process when the LAST handle closes, however
//! this process dies (a hard crash included: handle closure at process exit is the kernel's, not code of ours).
//! "Ends the tree" is scoped to in-job descendants: broker-mediated spawning (WMI, COM activation, schtasks,
//! services) is outside the domain.
#![cfg(windows)]

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// A job call that failed: the Win32 function and the OS error under it.
#[derive(Debug, thiserror::Error)]
#[error("{op}: {source}")]
pub struct JobError {
    pub op: &'static str,
    #[source]
    pub source: std::io::Error,
}

fn failed(op: &'static str) -> JobError {
    JobError { op, source: std::io::Error::last_os_error() }
}

#[derive(Debug)]
pub struct AnonymousJob(OwnedHandle);

impl AnonymousJob {
    pub fn create() -> Result<Self, JobError> {
        // NULL name + NULL security attributes: anonymous, non-inheritable by construction.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(failed("CreateJobObjectW"));
        }
        let owned = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
        // Zero-initialized is a valid `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`: every field is a plain integer, so an
        // all-zero bit pattern is a well-defined value; only `LimitFlags` needs a real one.
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
            return Err(failed("SetInformationJobObject"));
        }
        Ok(Self(owned))
    }

    /// The raw handle, for a caller that assigns a process to the job or probes it (`AssignProcessToJobObject`,
    /// `IsProcessInJob`).
    pub fn raw(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }

    /// `TerminateJobObject`: the raw call only; sequencing around it is the caller's.
    pub fn terminate(&self) -> Result<(), JobError> {
        if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
            return Err(failed("TerminateJobObject"));
        }
        Ok(())
    }

    /// `QueryInformationJobObject(JobObjectBasicAccountingInformation)`, the `ActiveProcesses` field, NOT
    /// `TotalProcesses`, which is a cumulative count that never decreases and would never observe "the tree is gone".
    pub fn active_processes(&self) -> Result<u32, JobError> {
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
            return Err(failed("QueryInformationJobObject"));
        }
        Ok(info.ActiveProcesses)
    }
}
