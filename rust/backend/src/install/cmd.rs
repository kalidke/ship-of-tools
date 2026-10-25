//! How the foreground `sotd install` runs its steps. Bounded output owns a group/job through output completion and
//! bounded cleanup. Other output/shown calls retain their existing waits and stop handling; the observation and
//! abnormal-exit limits are recorded in scripts/CLAUDE.md.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
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
/// printed (scripts/CLAUDE.md, Installers: "One verdict"). Bounded steps retain their containment owner until cleanup;
/// other steps use the existing Unix group or Windows Ctrl+C/tree stop path.
pub(super) struct Steps {
    running: Mutex<Vec<u32>>,
    stopping: AtomicBool,
    bounded: AtomicUsize,
    contained: OnceLock<&'static sot_containment::Signal>,
}

/// The run's steps.
pub(super) static STEPS: Steps = Steps::new();

impl Steps {
    pub(super) const fn new() -> Self {
        Self { running: Mutex::new(Vec::new()), stopping: AtomicBool::new(false), bounded: AtomicUsize::new(0), contained: OnceLock::new() }
    }

    fn running(&self) -> std::sync::MutexGuard<'_, Vec<u32>> {
        self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[allow(clippy::disallowed_methods, reason = "sotd install is a one-shot command a person runs in the foreground, not the daemon: its children are its own install steps, each waited for here (scripts/CLAUDE.md, Installers)")]
    fn start(&self, cmd: &mut Command) -> std::io::Result<std::process::Child> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }
        // The check, the spawn and the registration hold the list a stop reads, so a step either is in the list the
        // stop signals or never starts.
        let mut running = self.running();
        // A stop is under way: no step starts after it, and its verdict ends the run.
        while self.stopping.load(Ordering::SeqCst) {
            drop(running);
            std::thread::park();
            running = self.running();
        }
        let child = cmd.spawn()?;
        running.push(child.id());
        Ok(child)
    }

    /// Whether a stop is under way: the run prints nothing more but its verdict.
    pub(super) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn finished(&self, pid: u32) {
        self.running().retain(|p| *p != pid);
    }

    fn start_bounded(&self, cmd: &mut Command) -> std::io::Result<sot_containment::ContainedStd> {
        let mut running = self.running();
        while self.stopping.load(Ordering::SeqCst) {
            drop(running);
            std::thread::park();
            running = self.running();
        }
        // Containment starts suspended on Windows, assigns its anonymous job, then resumes; Unix retains the
        // unreaped leader's group identity. The registry lives for this foreground run, as its API requires.
        let signal = *self.contained.get_or_init(|| Box::leak(Box::new(sot_containment::Signal::new())));
        let child = signal.spawn_std(cmd)?;
        self.bounded.fetch_add(1, Ordering::SeqCst);
        Ok(child)
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
        let deadline = Instant::now() + bound;
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match self.start_bounded(cmd) {
            Ok(c) => c,
            Err(e) => return Ran { code: None, out: String::new(), err: format!("{}: {e}", program(cmd)) },
        };
        let _active = BoundedRun(&self.bounded);
        let (sent, done) = mpsc::channel();
        let pipes: [Box<dyn Read + Send>; 2] = [Box::new(child.stdout.take().expect("piped stdout")), Box::new(child.stderr.take().expect("piped stderr"))];
        for (index, mut pipe) in pipes.into_iter().enumerate() {
            let sent = sent.clone();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
                let _ = sent.send((index, result));
            });
        }
        drop(sent);
        let mut bodies = [None, None];
        let failure = loop {
            while let Ok((index, body)) = done.try_recv() { bodies[index] = Some(body); }
            if Instant::now() >= deadline {
                break Some(format!("{} did not finish within {} ms", program(cmd), bound.as_millis()));
            }
            if self.is_stopping() { break Some(format!("{} stopped", program(cmd))); }
            if bodies.iter().any(|body| matches!(body, Some(Err(_)))) { break Some(format!("{} output read failed", program(cmd))); }
            match child.exited(false) {
                Ok(true) if bodies.iter().all(Option::is_some) => break None,
                Ok(_) => std::thread::sleep(Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now()))),
                Err(error) => break Some(format!("{} exit observation failed: {error}", program(cmd))),
            }
        };
        // One cleanup budget covers termination, reap, and both readers. No blocking join or wait follows it.
        let cleanup = Instant::now() + Duration::from_secs(5);
        let status = end_group(child, cleanup);
        while bodies.iter().any(Option::is_none) && Instant::now() < cleanup {
            match done.recv_timeout(cleanup.saturating_duration_since(Instant::now())) {
                Ok((index, body)) => bodies[index] = Some(body),
                Err(_) => break,
            }
        }
        let mut failures: Vec<String> = failure.into_iter().collect();
        let code = match status {
            Ok(status) => status.code(),
            Err(error) => { failures.push(format!("cleanup: {error}")); None }
        };
        if bodies.iter().any(|body| !matches!(body, Some(Ok(_)))) { failures.push("output cleanup did not complete within 5 s".into()); }
        if !failures.is_empty() { return Ran { code: None, out: String::new(), err: failures.join("; ") }; }
        let [out, err] = bodies.map(|body| String::from_utf8_lossy(&body.unwrap().unwrap()).into_owned());
        Ran { code, out, err }
    }

    /// A person stopped the run: no step starts again, the running one is sent `signal` with every process it started,
    /// and after `grace` what is left is killed. Returns once the step has ended, or the grace and one second more have
    /// passed.
    pub(super) fn stop(&self, signal: i32, grace: Duration) {
        let groups: Vec<u32> = {
            let running = self.running();
            self.stopping.store(true, Ordering::SeqCst);
            running.clone()
        };
        signal_groups(&groups, signal);
        if let Some(contained) = self.contained.get() {
            if let Err(error) = contained.fire() { eprintln!("installer bounded child termination: {error}"); }
        }
        let t0 = Instant::now();
        while (!self.running().is_empty() || self.bounded.load(Ordering::SeqCst) != 0) && t0.elapsed() < grace {
            std::thread::sleep(Duration::from_millis(20));
        }
        // What is left is killed: on Unix its group; on Windows, where Ctrl+C was the signal, its process tree.
        signal_groups(&groups, KILL);
        #[cfg(windows)]
        kill_trees(&self.running().clone());
        let t1 = Instant::now();
        while (!self.running().is_empty() || self.bounded.load(Ordering::SeqCst) != 0) && t1.elapsed() < Duration::from_secs(1) {
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

/// End each step that outlived a stop's grace with every process under it (`taskkill /T /F`, a tool of every Windows).
#[cfg(windows)]
#[allow(clippy::disallowed_methods, reason = "the stop's last act: no step may start now, so the kill runs outside the steps and is waited for here")]
fn kill_trees(pids: &[u32]) {
    for pid in pids {
        let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

/// End a step that outlived its bound, with its group, and reap it.
fn end_group(mut child: sot_containment::ContainedStd, deadline: Instant) -> std::io::Result<std::process::ExitStatus> {
    let (sent, done) = mpsc::channel();
    std::thread::spawn(move || {
        let result = child.kill();
        drop(child);
        let _ = sent.send(result);
    });
    done.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "owned group/job did not end and reap within 5 s"))?
}

struct BoundedRun<'a>(&'a AtomicUsize);
impl Drop for BoundedRun<'_> {
    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::SeqCst); }
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

#[cfg(test)]
#[path = "cmd_tests.rs"]
mod tests;
