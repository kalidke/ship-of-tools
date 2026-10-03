// ancestors.rs — `sotd ancestors [--from <pid>]`: the ancestors of process
// <pid> (default: this one), parent first, one line each:
// `<pid>\t<exe file name>\t<full command line>`. Windows only. comm-lib.sh's
// `_sot_ancestor_chain` reads it to count how many agents lie between a comm
// script and its row's capsule. It reads Cygwin's /proc up to the first MSYS
// process whose parent is native and passes that process's Windows pid as
// --from: an MSYS program that execs another is a new Windows process whose
// Windows parent has exited, so a walk from sotd's own parent would stop at
// the script's own shell. Each line's pid is what lets the script go on
// through Cygwin's /proc where this walk stops. The command line is what tells
// `node <agent script>` from any other node; one that cannot be read is
// printed empty. A TAB inside a command line is printed raw (the pid and the
// exe end at the first two TABs); a newline and a return are printed as \x1c
// and \x1b, never a space.
//
// One Toolhelp snapshot, then a walk upward from
// the start process's parent. The snapshot keeps a parent pid after the parent has exited, and Windows reuses
// pids, so the walk stops where the chain stops being one, and that place is the
// top: a pid missing from the snapshot, pid 0 or 4 (System), a process whose
// record cannot be opened, one created after its child (a reused pid: the real
// parent has exited, the twin of reparenting to pid 1). Only a chain that goes on past 64
// lines is cut short: it ends with the line `!truncated` and exit 3, and the
// caller must not take what it has read for the whole chain.

/// The usage line, printed on a bad argument and for `--help`.
pub(crate) const USAGE: &str = "usage: sotd ancestors [--from <pid>]";
/// The most lines a walk prints.
const MAX_LINES: usize = 64;

/// What the walk does at one candidate parent.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Step {
    /// The candidate is not part of the chain: the walk is complete.
    Top,
    /// Print the candidate and go on from its parent; its creation time is the
    /// bound for the next one.
    Next(u64),
    /// The chain goes on past the cap.
    Truncated,
}

/// The walk's decision at candidate `pid`, after `printed` lines: `in_snapshot`
/// is whether the snapshot lists it, `created` its creation time (`None` when
/// its record cannot be opened or read), `child_created` its child's.
#[cfg_attr(not(windows), allow(dead_code))]
fn step(pid: u32, printed: usize, in_snapshot: bool, created: Option<u64>, child_created: u64) -> Step {
    if pid == 0 || pid == 4 || !in_snapshot {
        return Step::Top;
    }
    match created {
        Some(c) if c <= child_created => {
            if printed >= MAX_LINES {
                Step::Truncated
            } else {
                Step::Next(c)
            }
        }
        _ => Step::Top,
    }
}

/// A command line as it is printed on one `<exe>\t<command line>` line. A TAB
/// stays raw: the exe ends at the first TAB and the reader takes the rest, an
/// unquoted TAB being an argument separator. A newline would end the line and a
/// space would make two command lines that differ only by one equal, so a
/// newline and a return each have their own byte: \x1c and \x1b.
#[cfg_attr(not(windows), allow(dead_code))]
fn encode_command_line(text: &str) -> String {
    text.replace('\n', "\u{1c}").replace('\r', "\u{1b}")
}

/// `--from <pid>`, the process whose ancestors are printed, or `None` for this
/// one; any other arguments are a usage error.
fn parse_from(args: &[String]) -> Result<Option<u32>, String> {
    match args {
        [] => Ok(None),
        [flag, pid] if flag == "--from" => pid
            .parse()
            .map(Some)
            .map_err(|_| format!("--from takes a process id, not {pid:?}")),
        _ => Err(USAGE.to_string()),
    }
}

/// One printed line: the pid, the exe, then the command line, which alone may
/// hold a TAB.
#[cfg_attr(not(windows), allow(dead_code))]
fn line(pid: u32, exe: &str, cl: &str) -> String {
    format!("{pid}\t{exe}\t{cl}")
}

/// Runs the subcommand with the arguments after `ancestors` and returns its
/// exit status: 0 when the walk reached the top (no line at all when the start
/// process's parent is already the top), 3 when it was truncated, 1 when the
/// snapshot or the start process's record cannot be read, 2 for any other
/// arguments or off Windows.
pub fn run(args: &[String]) -> i32 {
    let from = match parse_from(args) {
        Ok(from) => from,
        Err(e) => {
            eprintln!("sotd ancestors: {e}");
            return 2;
        }
    };
    #[cfg(windows)]
    {
        win::run(from)
    }
    #[cfg(not(windows))]
    {
        let _ = from;
        eprintln!("sotd ancestors: Windows only");
        2
    }
}

#[cfg(windows)]
mod win {
    use super::{encode_command_line, line, step, Step};
    use std::collections::HashMap;
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

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

    /// A process's creation time and full command line, from one handle, or
    /// `None` when its record cannot be opened or its creation time read (the
    /// caller then treats it as the top). The command line is `None` when it
    /// cannot be read.
    fn info(pid: u32) -> Option<(u64, Option<String>)> {
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
            let created = sot_log::challenge_win::creation_filetime_bits(handle).ok();
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
            created.map(|c| (c, text.map(|t| encode_command_line(&t))))
        }
    }

    pub fn run(from: Option<u32>) -> i32 {
        let Some(procs) = snapshot() else { return 1 };
        let start = from.unwrap_or_else(std::process::id);
        let Some((mut child_created, _)) = info(start) else { return 1 };
        let Some(mut pid) = procs.get(&start).map(|p| p.0) else { return 1 };
        let mut printed = 0;
        let mut truncated = false;
        loop {
            let entry = procs.get(&pid);
            let found = info(pid);
            let created = found.as_ref().map(|f| f.0);
            match step(pid, printed, entry.is_some(), created, child_created) {
                Step::Top => break,
                Step::Truncated => {
                    truncated = true;
                    break;
                }
                Step::Next(c) => {
                    child_created = c;
                    let Some((parent, exe)) = entry else { break };
                    let cl = found.and_then(|f| f.1).unwrap_or_default();
                    println!("{}", line(pid, exe, &cl));
                    printed += 1;
                    pid = *parent;
                }
            }
        }
        if truncated {
            println!("!truncated");
            3
        } else {
            0
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

    #[test]
    fn a_tab_stays_raw_and_a_newline_and_a_return_each_have_their_own_byte() {
        assert_eq!(encode_command_line("a\tb"), "a\tb");
        assert_eq!(encode_command_line("a\nb"), "a\u{1c}b");
        assert_eq!(encode_command_line("a\rb"), "a\u{1b}b");
        assert_eq!(encode_command_line("a b"), "a b");
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_argument_starts_at_this_process_and_from_names_the_start() {
        assert_eq!(parse_from(&args(&[])), Ok(None));
        assert_eq!(parse_from(&args(&["--from", "4242"])), Ok(Some(4242)));
    }

    #[test]
    fn any_other_arguments_are_a_usage_error() {
        for bad in [&["--from"][..], &["--from", "x"], &["--from", "-1"], &["--from", "1", "2"], &["4242"], &["--to", "1"]] {
            assert!(parse_from(&args(bad)).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn a_line_is_the_pid_the_exe_and_the_command_line_whose_tabs_stay_raw() {
        assert_eq!(line(4242, "bash.exe", "bash.exe -c\tx"), "4242\tbash.exe\tbash.exe -c\tx");
        assert_eq!(line(7, "claude.exe", ""), "7\tclaude.exe\t");
    }
}
