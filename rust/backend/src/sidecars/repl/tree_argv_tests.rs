//! The command-line observer the REPL's page-secret and spawn controls use: every process of a child's tree, read the
//! way each OS can, and each one's command line. A process that has exited is skipped; a command line that cannot be
//! read from a process still alive fails the test.

#[cfg(any(windows, target_os = "macos"))]
use std::time::Duration;

/// The bound on one reading of the process table or of a command line (Linux reads `/proc`, with no child).
#[cfg(any(windows, target_os = "macos"))]
const BOUND: Duration = Duration::from_secs(30);

/// The running child's real argument vector, from the operating system, or `None` once the process has exited. On
/// Windows the OS keeps one command line, returned as a single element. On Windows a Julia child's arguments are not
/// observable this way: Julia's loader splits its own command line in place, so the reading stops at the executable
/// path.
#[cfg(target_os = "linux")]
fn os_argv(pid: u32) -> Option<Vec<String>> {
    let raw = match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(raw) => raw,
        Err(_) if !std::path::Path::new(&format!("/proc/{pid}")).exists() => return None,
        Err(e) => panic!("the command line of process {pid}, still alive, cannot be read: {e}"),
    };
    Some(
        raw.split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect(),
    )
}

#[cfg(target_os = "macos")]
fn os_argv(pid: u32) -> Option<Vec<String>> {
    // SAFETY: signal 0 only checks that the process exists.
    let gone = || unsafe {
        libc::kill(pid as libc::pid_t, 0) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    };
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    // SAFETY: the sizes and buffers passed are the ones the calls document.
    let first = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if first != 0 && gone() {
        return None;
    }
    assert_eq!(first, 0, "size the child's argument block");
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` is `size` bytes.
    let second = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if second != 0 && gone() {
        return None;
    }
    assert_eq!(second, 0, "read the child's argument block");
    buf.truncate(size);
    let argc = i32::from_ne_bytes(buf[..4].try_into().expect("argc")) as usize;
    // After the executable path and its padding come argc arguments.
    let rest = &buf[4..];
    let path_end = rest
        .iter()
        .position(|b| *b == 0)
        .expect("executable path end");
    let args_start = path_end
        + rest[path_end..]
            .iter()
            .position(|b| *b != 0)
            .expect("argument start");
    Some(
        rest[args_start..]
            .split(|b| *b == 0)
            .take(argc)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect(),
    )
}

#[cfg(windows)]
fn os_argv(pid: u32) -> Option<Vec<String>> {
    let mut command = std::process::Command::new("powershell");
    command.args([
        "-NoProfile",
        "-Command",
        &format!(
            "Get-CimInstance Win32_Process -Filter 'ProcessId={pid}' | ForEach-Object {{ if ($null -eq $_.CommandLine) {{ 'NULL ' + $_.Name + ' parent ' + $_.ParentProcessId + ' created ' + $_.CreationDate }} else {{ 'CMD ' + $_.CommandLine }} }}"
        ),
    ]);
    let ran = sot_platform::test_wait::run_within(&mut command, BOUND).ok();
    let row = ran.stdout.trim();
    if row.is_empty() {
        return None;
    }
    let line = row
        .strip_prefix("CMD ")
        .unwrap_or_else(|| panic!("the command line of process {pid}, still alive, cannot be read: {row}"));
    Some(vec![line.to_owned()])
}

/// The process ids of the child's tree, read the way each OS can. Linux and macOS: its process group (the containment
/// makes the child its leader), from `/proc` and from one `ps -A -o pid=,pgid=`. Windows: the root and every
/// descendant, walked from the root over one CIM query of `ProcessId, ParentProcessId, CreationDate` ([`descendants`]);
/// a descendant whose parent has already exited breaks the walk and is not seen (the daemon's job holds it, and the
/// Linux run covers that path).
#[cfg(target_os = "linux")]
fn read_tree(root: u32) -> Vec<u32> {
    let group = |pid: u32| -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = &stat[stat.rfind(')')? + 2..];
        after.split_whitespace().nth(2)?.parse().ok()
    };
    std::fs::read_dir("/proc")
        .expect("read the process table")
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| group(*pid) == Some(root))
        .collect()
}

#[cfg(target_os = "macos")]
fn read_tree(root: u32) -> Vec<u32> {
    let mut command = std::process::Command::new("ps");
    command.args(["-A", "-o", "pid=,pgid="]);
    let ran = sot_platform::test_wait::run_within(&mut command, BOUND).ok();
    ran.stdout
        .lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace().map(|c| c.parse::<u32>().ok());
            let (pid, group) = (cols.next()??, cols.next()??);
            (group == root).then_some(pid)
        })
        .collect()
}

#[cfg(windows)]
fn read_tree(root: u32) -> Vec<u32> {
    let mut command = std::process::Command::new("powershell");
    command.args([
        "-NoProfile",
        "-Command",
        "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId) $($_.ParentProcessId) $(if ($_.CreationDate) { $_.CreationDate.ToFileTimeUtc() } else { 0 })\" }",
    ]);
    let ran = sot_platform::test_wait::run_within(&mut command, BOUND).ok();
    let rows: Vec<(u32, u32, u64)> = ran
        .stdout
        .lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            Some((cols.next()?.parse().ok()?, cols.next()?.parse().ok()?, cols.next()?.parse().ok()?))
        })
        .collect();
    assert!(!rows.is_empty(), "the tree reader: the CIM query printed no process: {ran}");
    assert!(rows.iter().any(|row| row.0 == root), "the tree reader did not list the root {root}: {ran}");
    descendants(root, &rows)
}

/// The root and its descendants among `rows` (pid, parent pid, creation time). Windows never updates a
/// ParentProcessId and reuses process ids, so a process created before the process its ParentProcessId now names is not
/// that process's child: the walk follows an edge only when the child began at or after its parent.
fn descendants(root: u32, rows: &[(u32, u32, u64)]) -> Vec<u32> {
    let created = |pid: u32| rows.iter().find(|row| row.0 == pid).map(|row| row.2);
    let mut tree = vec![root];
    let mut next = 0;
    while next < tree.len() {
        let parent = tree[next];
        next += 1;
        let began = created(parent);
        for (pid, _, at) in rows.iter().filter(|row| row.1 == parent) {
            if began.is_some_and(|began| *at >= began) && !tree.contains(pid) {
                tree.push(*pid);
            }
        }
    }
    tree
}

/// The Windows tree walk adopts no process through a reused parent id: a process created before the process its
/// ParentProcessId names is not its child, nor is anything it started (rows as the CIM listing gives them: pid,
/// parent, creation).
#[test]
fn the_tree_walk_skips_a_process_older_than_its_named_parent() {
    let rows = [(10, 1, 100), (11, 10, 150), (12, 10, 50), (13, 11, 150), (14, 12, 200)];
    assert_eq!(descendants(10, &rows), [10, 11, 13]);
}

/// The tree's process ids, which must include the root: a reading that cannot list the root fails the test.
fn tree_pids(root: u32) -> Vec<u32> {
    let tree = read_tree(root);
    assert!(
        tree.contains(&root),
        "the tree reader did not list the root {root}"
    );
    tree
}

/// Whether any command line in the tree rooted at `root` carries any needle. Only booleans leave this function. A
/// process that has exited is skipped; a command line that cannot be read from a process still alive fails the test.
pub(super) fn tree_argv_leaks(root: u32, needles: &[&str]) -> bool {
    tree_pids(root).into_iter().any(|pid| {
        os_argv(pid).is_some_and(|argv| {
            argv.iter().any(|arg| {
                needles
                    .iter()
                    .any(|needle| !needle.is_empty() && arg.contains(needle))
            })
        })
    })
}
