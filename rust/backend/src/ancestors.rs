// ancestors.rs — `sotd ancestors`: this process's ancestor executables, parent
// first, one per line. Windows only. comm-lib.sh's `_sot_ancestor_chain` reads
// it where `ps` cannot see a native parent through an MSYS shell, to count how
// many agents lie between a comm script and its row's capsule.
//
// One Toolhelp snapshot, then a walk upward from this process's own parent. The
// snapshot keeps a parent pid after the parent has exited, and Windows reuses
// pids, so the walk stops where the chain stops being one: a pid missing from
// the snapshot, pid 0 or 4 (System), a repeat, 64 lines, or a parent whose
// creation time is unreadable or later than its child's.

/// Runs the subcommand and returns its exit status: 0 when at least one line
/// was printed, else 1; 2 off Windows.
pub fn run() -> i32 {
    #[cfg(windows)]
    {
        win::run()
    }
    #[cfg(not(windows))]
    {
        eprintln!("sotd ancestors: Windows only");
        2
    }
}

#[cfg(windows)]
mod win {
    use std::collections::{HashMap, HashSet};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    const MAX_LINES: usize = 64;

    /// pid -> (parent pid, executable file name), one snapshot.
    fn snapshot() -> Option<HashMap<u32, (u32, String)>> {
        // SAFETY: plain Win32 calls; `entry` is a stack-local out-param whose
        // `dwSize` is set before the first call, and the snapshot handle is
        // closed on every path out.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut map = HashMap::new();
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snap, &mut entry) != 0;
            while more {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
                map.insert(entry.th32ProcessID, (entry.th32ParentProcessID, exe));
                more = Process32NextW(snap, &mut entry) != 0;
            }
            CloseHandle(snap);
            Some(map)
        }
    }

    /// A process's creation time, or `None` when it cannot be read.
    fn created(pid: u32) -> Option<u64> {
        // SAFETY: `OpenProcess` returns null on failure; a non-null handle is
        // closed before returning.
        unsafe {
            let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let bits = sot_log::challenge_win::creation_filetime_bits(handle).ok();
            CloseHandle(handle);
            bits
        }
    }

    pub fn run() -> i32 {
        let Some(procs) = snapshot() else { return 1 };
        let me = std::process::id();
        let Some(mut child_created) = created(me) else { return 1 };
        let mut pid = procs.get(&me).map(|p| p.0).unwrap_or(0);
        let mut seen = HashSet::new();
        let mut printed = 0;
        while printed < MAX_LINES && pid != 0 && pid != 4 && seen.insert(pid) {
            let Some((parent, exe)) = procs.get(&pid) else { break };
            match created(pid) {
                Some(c) if c <= child_created => child_created = c,
                _ => break,
            }
            println!("{exe}");
            printed += 1;
            pid = *parent;
        }
        if printed > 0 {
            0
        } else {
            1
        }
    }
}
