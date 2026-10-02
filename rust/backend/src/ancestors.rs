// ancestors.rs — `sotd ancestors`: this process's ancestors, parent first, one
// line each: `<exe file name>\t<full command line>`. Windows only. comm-lib.sh's
// `_sot_ancestor_chain` reads it where `ps` cannot see a native parent through
// an MSYS shell, to count how many agents lie between a comm script and its
// row's capsule. The command line is what tells `node <agent script>` from any
// other node; one that cannot be read is printed empty.
//
// One Toolhelp snapshot, then a walk upward from this process's own parent. The
// snapshot keeps a parent pid after the parent has exited, and Windows reuses
// pids, so the walk stops where the chain stops being one: a pid missing from
// the snapshot or pid 0 or 4 (System) is the top. A walk cut short any other way
// (64 lines, a parent whose creation time is unreadable or later than its
// child's) ends with the line `!truncated` and exit 3: the caller must not take
// what it has read for the whole chain.

/// Runs the subcommand and returns its exit status: 0 when at least one line
/// was printed and the walk reached the top, 3 when it was truncated, else 1;
/// 2 off Windows.
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
    use std::collections::HashMap;
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING};
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

    /// A process's full command line, or `None` when it cannot be read. Tabs and
    /// line breaks become spaces: the caller reads one tab-separated line per
    /// process.
    fn command_line(pid: u32) -> Option<String> {
        // SAFETY: `OpenProcess` returns null on failure; a non-null handle is
        // closed before returning. The buffer is `u64` words, so the
        // `UNICODE_STRING` header at its start is aligned, and the query writes
        // the string's characters into the same buffer, which outlives the
        // slice read from it.
        unsafe {
            let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut need: u32 = 0;
            NtQueryInformationProcess(handle, ProcessCommandLineInformation, std::ptr::null_mut(), 0, &mut need);
            let text = if need == 0 {
                None
            } else {
                let mut buf = vec![0u64; (need as usize).div_ceil(8)];
                let status = NtQueryInformationProcess(
                    handle,
                    ProcessCommandLineInformation,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * 8) as u32,
                    &mut need,
                );
                let us = &*(buf.as_ptr() as *const UNICODE_STRING);
                if status < 0 || us.Buffer.is_null() {
                    None
                } else {
                    let wide = std::slice::from_raw_parts(us.Buffer, (us.Length / 2) as usize);
                    Some(String::from_utf16_lossy(wide))
                }
            };
            CloseHandle(handle);
            text.map(|t| t.replace(['\t', '\r', '\n'], " "))
        }
    }

    pub fn run() -> i32 {
        let Some(procs) = snapshot() else { return 1 };
        let me = std::process::id();
        let Some(mut child_created) = created(me) else { return 1 };
        let mut pid = procs.get(&me).map(|p| p.0).unwrap_or(0);
        let mut printed = 0;
        let mut truncated = false;
        while pid != 0 && pid != 4 {
            if printed >= MAX_LINES {
                truncated = true;
                break;
            }
            let Some((parent, exe)) = procs.get(&pid) else { break };
            match created(pid) {
                Some(c) if c <= child_created => child_created = c,
                _ => {
                    truncated = true;
                    break;
                }
            }
            println!("{exe}\t{}", command_line(pid).unwrap_or_default());
            printed += 1;
            pid = *parent;
        }
        if truncated {
            println!("!truncated");
            3
        } else if printed > 0 {
            0
        } else {
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parent_that_cannot_be_opened_is_the_top() {
        assert_eq!(step(500, 3, true, None, 100), Step::Top);
    }

    #[test]
    fn a_parent_created_after_its_child_is_the_top() {
        assert_eq!(step(500, 3, true, Some(101), 100), Step::Top);
    }

    #[test]
    fn a_parent_created_with_or_before_its_child_is_the_next_step() {
        assert_eq!(step(500, 3, true, Some(100), 100), Step::Next(100));
        assert_eq!(step(500, 3, true, Some(7), 100), Step::Next(7));
    }

    #[test]
    fn a_parent_missing_from_the_snapshot_and_the_system_pids_are_the_top() {
        assert_eq!(step(500, 3, false, Some(7), 100), Step::Top);
        assert_eq!(step(0, 3, true, Some(7), 100), Step::Top);
        assert_eq!(step(4, 3, true, Some(7), 100), Step::Top);
    }

    #[test]
    fn only_the_64_line_cap_is_truncation() {
        assert_eq!(step(500, 63, true, Some(7), 100), Step::Next(7));
        assert_eq!(step(500, 64, true, Some(7), 100), Step::Truncated);
        // the cap is for a chain that goes on; a top at line 64 is a whole walk
        assert_eq!(step(500, 64, true, None, 100), Step::Top);
    }
}
