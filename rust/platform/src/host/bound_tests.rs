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

/// A child that is meant to crash writes no core file: a limit of 1 byte also stops a piped dump (a limit of 0 does
/// not start one on every kernel, but 1 is the value that never does).
#[cfg(unix)]
fn no_core_file() {
    let none = libc::rlimit { rlim_cur: 1, rlim_max: 1 };
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
        assert_eq!((role, status.code()), (role, Some(CODE)));
    }
}

#[cfg(unix)]
#[test]
fn a_process_past_its_cpu_limit_exits_with_its_code() {
    const NAME: &str = "host::bound::tests::a_process_past_its_cpu_limit_exits_with_its_code";
    if child_role().is_some() {
        enter(NAME);
        no_core_file();
        limit_cpu(1, CODE).unwrap();
        loop {
            std::hint::black_box(0u64);
        }
    }
    assert_eq!(run_child(NAME, "spin").0.code(), Some(CODE));
}

/// The child's parent exits and the child, which asked to end with it, is gone: it held the write end of a pipe the
/// test reads, so the pipe's end of file is the child's end, and a child that stays ends it only by its own sleep.
#[cfg(target_os = "linux")]
#[test]
fn a_child_ends_with_its_parent() {
    use std::io::{BufRead, BufReader, Read};
    const NAME: &str = "host::bound::tests::a_child_ends_with_its_parent";
    match child_role().as_deref() {
        Some("orphan") => {
            end_with_parent().unwrap();
            println!("asked");
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
                .stderr(Stdio::inherit()); // the test's pipe, as this process's stderr
            let mut orphan = command.spawn().unwrap();
            let lines = BufReader::new(orphan.stdout.take().unwrap()).lines();
            // The parent ends only after the child has asked.
            assert!(lines.map_while(Result::ok).any(|line| line.contains("asked")), "the child never asked");
        }
        None => {
            let (mut held_by_children, write) = std::io::pipe().unwrap();
            let (mut command, entry) = test_command(NAME);
            let mut parent = command
                .env(ROLE, "parent")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(write)
                .spawn()
                .unwrap();
            drop(command); // the test's own copy of the write end
            wait_within(&mut parent, ISOLATION_TIMEOUT);
            entry.assert_once(parent.id());
            let waited = std::time::Instant::now();
            held_by_children.read_to_end(&mut Vec::new()).unwrap();
            assert!(waited.elapsed() < std::time::Duration::from_secs(10), "the child outlived its parent");
        }
    }
}

#[cfg(windows)]
#[test]
fn exit_on_crash_sets_the_windows_error_mode() {
    use windows_sys::Win32::System::Diagnostics::Debug::{GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX};
    if !run_isolated("host::bound::tests::exit_on_crash_sets_the_windows_error_mode") {
        return;
    }
    let both = SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX;
    exit_on_crash(CODE).unwrap();
    assert_eq!(unsafe { GetErrorMode() } & both, both);
}

/// After `take_stdout`, every route to stdout the process has is dead and the private file is the one that reaches the
/// parent: Rust's `print!`, a raw write to descriptor 1 (Windows: the standard output handle and the C runtime's), and
/// the private file's marker arrive as the marker alone. The harness's own opening line precedes the body.
#[test]
fn only_the_private_copy_reaches_the_parents_stdout() {
    use std::io::Write;
    const NAME: &str = "host::bound::tests::only_the_private_copy_reaches_the_parents_stdout";
    const MARKER: &str = "<the marker>";
    if child_role().is_some() {
        enter(NAME);
        let mut private = take_stdout().unwrap();
        print!("stray print ");
        println!("stray println");
        std::io::stdout().flush().unwrap();
        let stray = b"stray write ";
        #[cfg(unix)]
        // SAFETY: a plain write of a live buffer to descriptor 1.
        unsafe {
            libc::write(1, stray.as_ptr().cast(), stray.len());
        }
        #[cfg(windows)]
        // SAFETY: a plain write of a live buffer to the standard output handle and to the runtime's descriptor 1.
        unsafe {
            use windows_sys::Win32::Storage::FileSystem::WriteFile;
            use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
            extern "C" {
                fn _write(fd: i32, buffer: *const u8, count: u32) -> i32;
            }
            let mut written = 0;
            WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), stray.as_ptr(), stray.len() as u32, &mut written, std::ptr::null_mut());
            _write(1, stray.as_ptr(), stray.len() as u32);
        }
        private.write_all(MARKER.as_bytes()).unwrap();
        return;
    }
    let (status, out) = run_child(NAME, "take");
    assert_eq!(status.code(), Some(0));
    assert!(!out.contains("stray"), "a stray write reached the parent: {out:?}");
    assert!(out.ends_with(MARKER), "the private copy's marker did not arrive last: {out:?}");
}
