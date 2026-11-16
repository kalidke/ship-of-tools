//! The test reaper (Linux): one process for each `Env`, outside its test's process tree, session and group, that ends
//! what the `Env` started when the test's process ends without `Env`'s `Drop` (killed by a timeout or a signal, or
//! aborted). A row's capsule outlives its daemon by design, and one a user manager scopes lives outside the test's
//! control group too, so nothing else ends it.
//!
//! The reaper is this test binary rerun for [`NAME`] alone (`test_isolated`). Its standard input is a pipe whose one
//! writer is the `Env`: the `Env` tells it its folders, its state root, its daemon socket, each daemon it launches and
//! each user unit it starts, and the pipe's end (the `Env` dropped, or its process gone however it went) is the cue.
//! The reaper then does what `Drop` does: it stops each unit, ends each daemon whose command line still names the
//! `Env`'s socket (TERM, which its guard forwards, then KILL past `STOP_BOUND`; the identity is read after the pidfd is
//! opened), sweeps the capsules over the state root ([`sweep_capsules`]) and removes the folders. After an ordinary end
//! it finds all of that done. What it could not end it names in `sot-test-reaper.txt` in the `Env`'s folder, which it
//! then keeps.
//!
//! Limits: a kill of the test's whole control group (a test container's end, a stop of the unit or scope the test runs
//! in) takes the reaper too; rows in that group end with it, rows a user manager scoped outside it do not. A child the
//! test forked and has not yet exec'd holds a copy of the pipe's writer, which delays the cue until it execs or ends. A
//! process a row's agent started outside its process group is not ended, as by `Drop`.

use super::*;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;

/// The reaper's exact libtest name in every binary that declares `mod support;`.
const NAME: &str = "support::reaper::reaper";
/// Set only on the reaper's own command: the one run of [`reaper`] that does anything.
const ARM: &str = "SOT_TEST_REAPER";
/// The file in the `Env`'s folder that names what the reaper could not end.
const ACCOUNT: &str = "sot-test-reaper.txt";

/// The `Env`'s end of the reaper's pipe. Its drop, or the end of this process, is the reaper's cue.
pub struct Reaper(std::process::ChildStdin);

impl Reaper {
    /// Starts the reaper of an `Env` whose folders are `folders` (the first takes the account), whose capsules run over
    /// `state_root` and whose daemons serve `socket`, and returns once the reaper's body has entered.
    #[allow(dead_code, reason = "only the suites that make an Env start a reaper")]
    pub fn start(folders: &[&Path], state_root: &Path, socket: &Path) -> Reaper {
        let (mut command, entry) = sot_platform::test_isolated::test_command(NAME);
        command.arg("--ignored").env(ARM, "1").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        // SAFETY: fork, setsid and _exit are async-signal-safe, and the closure allocates nothing. The child that runs
        // it forks the reaper and exits at once, so the reaper's parent is this process's nearest subreaper, not this
        // process, and its session is its own: a kill of the test's process tree, group or session does not reach it.
        unsafe {
            command.pre_exec(|| match libc::fork() {
                -1 => Err(std::io::Error::last_os_error()),
                0 => {
                    libc::setsid();
                    Ok(())
                }
                _ => libc::_exit(0),
            });
        }
        let mut child = command.spawn().unwrap_or_else(|error| panic!("start the test reaper: {error}"));
        let writer = child.stdin.take().expect("the reaper's pipe");
        let launched = sot_platform::test_isolated::wait_within(&mut child, BOUND);
        assert!(launched.success(), "the test reaper's launch ended {launched}");
        let reaper = Reaper(writer);
        for folder in folders {
            reaper.tell("folder", folder.as_os_str());
        }
        reaper.tell("root", state_root.as_os_str());
        reaper.tell("socket", socket.as_os_str());
        // The entry record names the reaper's pid once its body has entered; a name that matches no test never does.
        let record = PathBuf::from(entry.environment_assignment().1);
        let deadline = Instant::now() + BOUND;
        let pid = loop {
            let text = std::fs::read_to_string(&record).unwrap_or_default();
            if let Some(pid) = text.strip_suffix('\n').and_then(|line| line.rsplit(' ').next()?.parse().ok()) {
                break pid;
            }
            assert!(Instant::now() < deadline, "the test reaper did not enter {NAME} within {BOUND:?}");
            std::thread::sleep(Duration::from_millis(5));
        };
        entry.assert_once(pid);
        reaper
    }

    /// The daemon `pid` this `Env` just launched (the process its spawn returned: on Linux the daemon's guard).
    pub fn daemon(&self, pid: u32) {
        self.tell("daemon", OsStr::new(&pid.to_string()));
    }

    /// The user unit this `Env` is about to start.
    #[allow(dead_code, reason = "only the suites that start a daemon as a user service tell one")]
    pub fn unit(&self, unit: &str) {
        self.tell("unit", OsStr::new(unit));
    }

    /// One record: `key`, a space, `value`, a NUL, in one write (shorter than the pipe's atomic size).
    fn tell(&self, key: &str, value: &OsStr) {
        let mut record = key.as_bytes().to_vec();
        record.push(b' ');
        record.extend_from_slice(value.as_bytes());
        record.push(0);
        (&self.0).write_all(&record).unwrap_or_else(|error| panic!("tell the test reaper the {key}: {error}"));
    }
}

/// What an `Env` told its reaper.
#[derive(Default)]
struct Told {
    folders: Vec<PathBuf>,
    root: Option<PathBuf>,
    socket: Option<PathBuf>,
    daemons: Vec<u32>,
    units: Vec<String>,
}

fn told(bytes: &[u8]) -> Told {
    let mut told = Told::default();
    for record in bytes.split(|b| *b == 0) {
        let Some(space) = record.iter().position(|b| *b == b' ') else { continue };
        let (key, value) = (&record[..space], &record[space + 1..]);
        let path = || PathBuf::from(OsString::from_vec(value.to_vec()));
        match key {
            b"folder" => told.folders.push(path()),
            b"root" => told.root = Some(path()),
            b"socket" => told.socket = Some(path()),
            b"daemon" => told.daemons.extend(std::str::from_utf8(value).ok().and_then(|v| v.parse::<u32>().ok())),
            b"unit" => told.units.push(String::from_utf8_lossy(value).into_owned()),
            _ => {}
        }
    }
    told
}

/// Ends the daemon `pid` while it is one of this `Env`'s, as [`end_daemon`] ends one: TERM, which its guard forwards
/// to the daemon, then KILL once `STOP_BOUND` has passed. Its command line is read after its pidfd is opened, so a
/// number the kernel has given another process is never signalled; only one that names `socket` after `--socket` is
/// this `Env`'s. A daemon already gone is ended.
fn end_daemon_of(pid: u32, socket: &Path) -> Result<(), String> {
    use sot_platform::identity::challenge_unix::{pidfd_open, poll_pidfd_readable};
    use std::os::fd::AsRawFd;
    let fd = match pidfd_open(pid) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
        Err(error) => return Err(format!("pidfd_open({pid}): {error}")),
    };
    let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) => cmdline,
        Err(error) if is_gone_error(&error) => return Ok(()),
        Err(error) => return Err(format!("read /proc/{pid}/cmdline after opening its pidfd: {error}")),
    };
    let argv: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
    if !argv.windows(2).any(|w| w[0] == b"--socket" && w[1] == socket.as_os_str().as_bytes()) {
        return Ok(());
    }
    for (signal, bound) in [(libc::SIGTERM, super::sotd::STOP_BOUND), (libc::SIGKILL, BOUND)] {
        let no_info = std::ptr::null::<libc::c_void>();
        // SAFETY: pidfd_send_signal takes a pidfd, a signal number, a null siginfo and flags 0.
        let rc = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), signal, no_info, 0) };
        if rc < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(format!("pidfd_send_signal({pid}, {signal}): {error}"));
        }
        match poll_pidfd_readable(fd.as_raw_fd(), bound) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return Err(format!("wait for daemon {pid} to end: {error}")),
        }
    }
    Err(format!("daemon {pid} still lives {BOUND:?} after SIGKILL"))
}

/// What `Drop` does, in its order: the units, the daemons, the capsules. Returns what it could not end.
fn reap(told: &Told) -> Vec<String> {
    let mut left = Vec::new();
    for unit in &told.units {
        let mut stop = Command::new("systemctl");
        stop.args(["--user", "stop"]).arg(unit);
        if let Err(error) = sot_platform::test_wait::try_run_within(&mut stop, BOUND) {
            left.push(format!("stop {unit}: {error}"));
        }
    }
    if let Some(socket) = &told.socket {
        for &pid in &told.daemons {
            if let Err(error) = end_daemon_of(pid, socket) {
                left.push(error);
            }
        }
    }
    if let Some(root) = &told.root {
        if let Err(error) = sweep_capsules(root) {
            left.push(error);
        }
    }
    left
}

/// The reaper's body (the module doc); a no-op in every run but the one [`Reaper::start`] starts.
#[test]
#[ignore = "the test reaper: an Env starts it as a process of its own"]
fn reaper() {
    if std::env::var_os(ARM).is_none() {
        return;
    }
    sot_platform::test_isolated::enter(NAME);
    let mut bytes = Vec::new();
    // Returns at the pipe's end: the Env dropped, or its process gone.
    let _ = std::io::stdin().lock().read_to_end(&mut bytes);
    let told = told(&bytes);
    let left = reap(&told);
    if left.is_empty() {
        for folder in told.folders.iter().filter(|folder| folder.is_absolute()) {
            let _ = std::fs::remove_dir_all(folder);
        }
    } else if let Some(folder) = told.folders.first() {
        let _ = std::fs::write(folder.join(ACCOUNT), left.join("\n") + "\n");
    }
}
