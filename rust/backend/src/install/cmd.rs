//! How `sotd install` runs a program: the one place it starts one. It is a command a person or an agent runs in the
//! foreground, not the daemon, so its children are the steps of the install, waited for here; none outlives the run.

use std::path::Path;
use std::process::{Command, Stdio};
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

#[allow(clippy::disallowed_methods, reason = "sotd install is a one-shot command a person runs in the foreground, not the daemon: its children are its own install steps, each waited for here (scripts/CLAUDE.md, Installers)")]
fn start(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    cmd.spawn()
}

/// Run `cmd` to its end with its output read; a program that cannot start is a result (no code, the reason in `err`).
pub(super) fn output(cmd: &mut Command) -> Ran {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    match start(cmd).and_then(std::process::Child::wait_with_output) {
        Ok(o) => Ran {
            code: o.status.code(),
            out: String::from_utf8_lossy(&o.stdout).into_owned(),
            err: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => Ran { code: None, out: String::new(), err: format!("{}: {e}", program(cmd)) },
    }
}

/// Run `cmd` to its end with stdout and stderr shown as they come (a clone's or an instantiate's progress): its exit
/// code, `None` when it did not start (the reason printed) or a signal ended it.
pub(super) fn shown(cmd: &mut Command) -> Option<i32> {
    cmd.stdin(Stdio::null());
    match start(cmd).and_then(|mut c| c.wait()) {
        Ok(status) => status.code(),
        Err(e) => {
            println!("{}: {e}", program(cmd));
            None
        }
    }
}

/// [`output`] within `bound` by the clock: at the bound the program is killed and the result has no code and says so.
#[cfg_attr(windows, allow(dead_code, reason = "the Unix role question's ssh probe"))]
pub(super) fn output_within(cmd: &mut Command, bound: Duration) -> Ran {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match start(cmd) {
        Ok(c) => c,
        Err(e) => return Ran { code: None, out: String::new(), err: format!("{}: {e}", program(cmd)) },
    };
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < bound => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Ran { code: None, out: String::new(), err: format!("{} did not finish within {}s", program(cmd), bound.as_secs()) };
            }
        }
    }
    match child.wait_with_output() {
        Ok(o) => Ran {
            code: o.status.code(),
            out: String::from_utf8_lossy(&o.stdout).into_owned(),
            err: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => Ran { code: None, out: String::new(), err: format!("{}: {e}", program(cmd)) },
    }
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
