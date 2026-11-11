use super::*;
use crate::test_isolated::{enter, run_isolated, test_command, wait_within, ISOLATION_TIMEOUT};
use std::process::{ExitStatus, Stdio};

/// The code every bound ends its child with here.
pub(crate) const CODE: i32 = 87;
const ROLE: &str = "SOT_TEST_BOUND_ROLE";

/// Set, to its role, in a child `run_child` started: the test's body then does the child's part.
pub(crate) fn child_role() -> Option<String> {
    std::env::var(ROLE).ok()
}

/// Re-runs `test` (its exact libtest name) as a child for `role`, which must call `enter(test)` first, and returns how
/// it ended with what it wrote to stdout. A body that ends its own process by a bound is not `run_isolated`'s
/// (that wants exit 0); the entry record is the same one.
pub(crate) fn run_child(test: &str, role: &str) -> (ExitStatus, String) {
    let (mut command, entry) = test_command(test);
    let mut child = command
        .env(ROLE, role)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the child");
    let status = wait_within(&mut child, ISOLATION_TIMEOUT);
    entry.assert_once(child.id());
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut out).unwrap();
    (status, out)
}

/// A child that is meant to crash writes no core file.
#[cfg(unix)]
fn no_core_file() {
    let none = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: a plain setrlimit on this process.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &none) }, 0);
}

#[test]
fn exit_now_ends_with_its_code() {
    const NAME: &str = "host::bound::tests::exit_now_ends_with_its_code";
    if child_role().is_some() {
        enter(NAME);
        exit_now(CODE);
    }
    assert_eq!(run_child(NAME, "exit").0.code(), Some(CODE));
}

#[cfg(unix)]
#[test]
fn close_inherited_leaves_no_descriptor_above_stderr() {
    if !run_isolated("host::bound::tests::close_inherited_leaves_no_descriptor_above_stderr") {
        return;
    }
    let open = || {
        let path = std::ffi::CString::new("/dev/null").unwrap();
        // SAFETY: a plain open without close-on-exec, the descriptor an exec'd child would inherit.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
        assert!(fd > 2);
        fd
    };
    // SAFETY: fcntl on a number is EBADF when it is not open.
    let is_open = |fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1;
    let by_loop = open();
    close_to_table_size();
    assert!(!is_open(by_loop), "the loop left a descriptor open");
    let by_call = open();
    close_inherited();
    assert!(!is_open(by_call), "close_inherited left a descriptor open");
    assert!(is_open(2), "stderr must stay");
}

#[cfg(unix)]
#[test]
fn an_abort_after_exit_on_crash_is_an_exit() {
    use std::os::unix::process::ExitStatusExt;
    const NAME: &str = "host::bound::tests::an_abort_after_exit_on_crash_is_an_exit";
    fn recurse(depth: u64) -> u64 {
        if std::hint::black_box(true) { recurse(depth + 1) + 1 } else { depth }
    }
    if let Some(role) = child_role() {
        enter(NAME);
        no_core_file();
        exit_on_crash(CODE).unwrap();
        if role == "abort" {
            std::process::abort();
        }
        let _ = std::thread::Builder::new().stack_size(256 << 10).spawn(|| recurse(0)).unwrap().join();
        std::process::exit(0);
    }
    for role in ["abort", "stack overflow"] {
        let status = run_child(NAME, role).0;
        assert_eq!((role, status.code(), status.core_dumped()), (role, Some(CODE), false));
    }
}

#[cfg(unix)]
#[test]
fn a_process_past_its_cpu_limit_exits_with_its_code() {
    use std::os::unix::process::ExitStatusExt;
    const NAME: &str = "host::bound::tests::a_process_past_its_cpu_limit_exits_with_its_code";
    if child_role().is_some() {
        enter(NAME);
        no_core_file();
        limit_cpu(1, CODE).unwrap();
        loop {
            std::hint::black_box(0u64);
        }
    }
    let status = run_child(NAME, "spin").0;
    assert_eq!((status.code(), status.core_dumped()), (Some(CODE), false));
}

/// The child's parent exits and the child, which asked to end with it, is gone.
#[cfg(target_os = "linux")]
#[test]
fn a_child_ends_with_its_parent() {
    use std::io::{BufRead, BufReader};
    const NAME: &str = "host::bound::tests::a_child_ends_with_its_parent";
    let said = |out: &mut dyn BufRead| {
        out.lines().map_while(Result::ok).find_map(|line| line.split_once("pid ").map(|(_, pid)| pid.trim().to_owned()))
    };
    match child_role().as_deref() {
        Some("orphan") => {
            end_with_parent().unwrap();
            println!("pid {}", std::process::id());
            std::thread::sleep(std::time::Duration::from_secs(20));
        }
        Some(_) => {
            enter(NAME);
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(ROLE, "orphan")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let mut orphan = command.spawn().unwrap();
            let pid = said(&mut BufReader::new(orphan.stdout.take().unwrap())).expect("the child never said");
            println!("pid {pid}"); // the parent ends after the child asked, and reports it
        }
        None => {
            let out = run_child(NAME, "parent").1;
            let pid: i32 = said(&mut out.as_bytes()).expect("no pid").parse().unwrap();
            let alive = || {
                std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z"))
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while alive() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let still = alive();
            if still {
                // SAFETY: ends the stray child this test made.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            assert!(!still, "the child outlived its parent");
        }
    }
}

#[cfg(windows)]
#[test]
fn exit_on_crash_sets_the_error_mode_so_no_dialog_stops_the_end() {
    use windows_sys::Win32::System::Diagnostics::Debug::{GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX};
    if !run_isolated("host::bound::tests::exit_on_crash_sets_the_error_mode_so_no_dialog_stops_the_end") {
        return;
    }
    let both = SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX;
    exit_on_crash(CODE).unwrap();
    assert_eq!(unsafe { GetErrorMode() } & both, both);
}
