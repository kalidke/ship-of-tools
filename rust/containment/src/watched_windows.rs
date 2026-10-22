//! Test observer (feature `test-support`, Windows): a retained process handle, read for the process's end, never
//! through a pid number Windows may have reused.

use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

pub struct Watched(usize);

impl Watched {
    pub fn open(pid: u32) -> Self {
        // SAFETY: the fixture is alive and owned; this retained handle is closed by Drop.
        let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
        assert!(
            !handle.is_null(),
            "cannot retain the fixture identity: {}",
            std::io::Error::last_os_error()
        );
        Self(handle as usize)
    }
    pub fn dead(&self) -> bool {
        self.dead_within(3000)
    }
    pub fn dead_within(&self, ms: u32) -> bool {
        // SAFETY: wait on this object's retained handle, rather than a potentially reused PID.
        unsafe { WaitForSingleObject(self.0 as _, ms) == WAIT_OBJECT_0 }
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        // SAFETY: close this object's own process handle.
        unsafe { CloseHandle(self.0 as _) };
    }
}