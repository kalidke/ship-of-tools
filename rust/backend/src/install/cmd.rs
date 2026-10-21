//! How `sotd install` runs a program: the one place it starts one. It is a command a person or an agent runs in the
//! foreground, not the daemon, so its children are the steps of the install, waited for here; none outlives the run,
//! a stopped one included ([`Steps::stop`]).

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What a finished program said: its exit code (`None` when it did not start or a signal ended it), stdout and stderr.
pub(super) struct Ran {
    pub code: Option<i32>,
    pub out: String,
    pub err: String,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// The first line of stdout, trimmed.
    pub fn first_line(&self) -> String {
        self.out.lines().next().unwrap_or("").trim().to_string()
    }

    /// The last line it printed that is not blank, stderr first: a failed program says why there.
    pub fn last_said(&self) -> String {
        let last = |t: &str| t.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(String::from);
        last(&self.err).or_else(|| last(&self.out)).unwrap_or_default()
    }
}

/// The steps a run starts, so a stop can end the one running, and every process it started, before the verdict is
/// printed (scripts/CLAUDE.md, Installers: "One verdict"). On Unix each step leads a process group of its own, which a
/// stop signals whole; on Windows Ctrl+C reaches the console's every process, and a stop waits for the step to end.
pub(super) struct Steps {
    running: Mutex<Vec<u32>>,
    stopping: AtomicBool,
}

/// The run's steps.
pub(super) static STEPS: Steps = Steps::new();

impl Steps {
    pub(super) const fn new() -> Self {
        Self { running: Mutex::new(Vec::new()), stopping: AtomicBool::new(false) }
    }

    fn running(&self) -> std::sync::MutexGuard<'_, Vec<u32>> {
        self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[allow(clippy::disallowed_methods, reason = "sotd install is a one-shot command a person runs in the foreground, not the daemon: its children are its own install steps, each waited for here (scripts/CLAUDE.md, Installers)")]
    fn start(&self, cmd: &mut Command) -> std::io::Result<std::process::Child> {
        // A stop is under way: no step starts after it, and its verdict ends the run.
        while self.stopping.load(Ordering::SeqCst) {
            std::thread::park();
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }
        let child = cmd.spawn()?;
        self.running().push(child.id());
        Ok(child)
    }

    /// Whether a stop is under way: the run prints nothing more but its verdict.
    pub(super) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn finished(&self, pid: u32) {
        self.running().retain(|p| *p != pid);
    }

    /// Run `cmd` to its end with its output read; a program that cannot start is a result (no code, the reason in
    /// `err`).
    #[allow(clippy::disallowed_methods, reason = "the one waiter of sotd install's own steps (the test rule is for a test's own children)")]
    pub(super) fn output(&self, cmd: &mut Command) -> Ran {
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = match self.start(cmd) {
            Ok(c) => c,
            Err(e) => return Ran { code: None, out: String::new(), err: format!("{}: {e}", program(cmd)) },
        };
        let pid = child.id();
        let done = child.wait_with_output();
        self.finished(pid);
        match done {
            Ok(o) => Ran { code: o.status.code(), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() },
            Err(e) => Ran { code: None, out: String::new(), err: format!("{}: {e}", program(cmd)) },
        }
    }

    /// Run `cmd` to its end with stdout and stderr shown as they come: its exit code, `None` when it did not start (the
    /// reason printed) or a signal ended it.
    pub(super) fn shown(&self, cmd: &mut Command) -> Option<i32> {
        cmd.stdin(Stdio::null());
        match self.start(cmd) {
            Ok(mut child) => {
                let pid = child.id();
                let status = child.wait();
                self.finished(pid);
                status.ok().and_then(|s| s.code())
            }
            Err(e) => {
                println!("{}: {e}", program(cmd));
                None
            }
        }
    }

    /// [`Steps::output`] within `bound` by the clock: at the bound the step is killed with what it started, and the
    /// result has no code and says so.
    #[allow(clippy::disallowed_methods, reason = "the one waiter of sotd install's own steps (the test rule is for a test's own children)")]
    pub(super) fn output_within(&self, cmd: &mut Command, bound: Duration) -> Ran {
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match self.start(cmd) {
            Ok(c) => c,
            Err(e) => return Ran { code: None, out: String::new(), err: format!("{}: {e}", program(cmd)) },
        };
        let pid = child.id();
        let t0 = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if t0.elapsed() < bound => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    end_group(pid, &mut child);
                    self.finished(pid);
                    return Ran { code: None, out: String::new(), err: format!("{} did not finish within {} ms", program(cmd), bound.as_millis()) };
                }
            }
        }
        let done = child.wait_with_output();
        self.finished(pid);
        match done {
            Ok(o) => Ran { code: o.status.code(), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() },
            Err(e) => Ran { code: None, out: String::new(), err: format!("{}: {e}", program(cmd)) },
        }
    }

    /// A person stopped the run: no step starts again, the running one is sent `signal` with every process it started,
    /// and after `grace` what is left is killed. Returns once the step has ended, or the grace and one second more have
    /// passed.
    pub(super) fn stop(&self, signal: i32, grace: Duration) {
        self.stopping.store(true, Ordering::SeqCst);
        let groups: Vec<u32> = self.running().clone();
        signal_groups(&groups, signal);
        let t0 = Instant::now();
        while !self.running().is_empty() && t0.elapsed() < grace {
            std::thread::sleep(Duration::from_millis(20));
        }
        signal_groups(&groups, KILL);
        let t1 = Instant::now();
        while !self.running().is_empty() && t1.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(unix)]
const KILL: i32 = libc::SIGKILL;
#[cfg(not(unix))]
const KILL: i32 = 9;

/// Signal each step's process group: the step and every process it started that stayed in it.
fn signal_groups(groups: &[u32], signal: i32) {
    #[cfg(unix)]
    for g in groups {
        if let Ok(g) = libc::pid_t::try_from(*g) {
            // SAFETY: kill(2) on a process group this run made (the step leads it); a group already gone is ESRCH.
            unsafe { libc::kill(-g, signal) };
        }
    }
    #[cfg(not(unix))]
    let _ = (groups, signal);
}

/// End a step that outlived its bound, with its group, and reap it.
fn end_group(pid: u32, child: &mut std::process::Child) {
    signal_groups(&[pid], KILL);
    let _ = child.kill();
    let _ = child.wait();
}

/// Run `cmd` as one of the run's steps; see [`Steps::output`].
pub(super) fn output(cmd: &mut Command) -> Ran {
    STEPS.output(cmd)
}

/// See [`Steps::shown`].
pub(super) fn shown(cmd: &mut Command) -> Option<i32> {
    STEPS.shown(cmd)
}

/// See [`Steps::output_within`].
pub(super) fn output_within(cmd: &mut Command, bound: Duration) -> Ran {
    STEPS.output_within(cmd, bound)
}

fn program(cmd: &Command) -> String {
    cmd.get_program().to_string_lossy().into_owned()
}

/// git with the stall bound on every call: a transfer under 1000 B/s for 60 s ends, and the pair reaches the lazy fetch
/// a blobless clone's `cat-file` or `worktree add` runs. A pair on the command line, never in the environment, so
/// nothing reaches the daemon and rows the install starts.
pub(super) fn git() -> Command {
    let mut c = Command::new(if cfg!(windows) { "git.exe" } else { "git" });
    c.args(["-c", "http.lowSpeedLimit=1000", "-c", "http.lowSpeedTime=60"]);
    c
}

/// git run in `dir` (`git -C <dir>`).
pub(super) fn git_in(dir: &Path) -> Command {
    let mut c = git();
    c.arg("-C").arg(dir);
    c
}

/// curl's bound on a whole transfer: a connect past 15 s, or under 1000 B/s for 60 s, ends it.
#[cfg_attr(windows, allow(dead_code, reason = "the Unix juliaup install"))]
pub(super) const CURL_BOUND: [&str; 6] = ["--connect-timeout", "15", "--speed-limit", "1000", "--speed-time", "60"];

/// Whether a program by this name is on PATH.
pub(super) fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    let exts: &[&str] = if cfg!(windows) { &["", ".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::split_paths(&path).any(|d| exts.iter().any(|e| d.join(format!("{name}{e}")).is_file()))
}
