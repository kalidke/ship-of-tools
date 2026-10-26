//! What the install asks of the computer, and the one way an answer is read (scripts/CLAUDE.md, Installers: "Bounded
//! waits"). A question is a read-only child the run decides on: [`ask`] runs it within its bound and gives a [`Fact`],
//! its value or why there is none, which no caller can read without saying what the unknown means. The [`Machine`] is
//! every child the install starts and the environment it reads, so a test can stand in for any one of them.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use sot_platform::host::env::Env;

use super::cmd::{self, Ran};

/// The bound on a question asked of the machine (a process listing, a `--version`, a unit's state). A cold CIM listing
/// takes about 26 s on a hosted Windows runner and misses 15 s in every environment, so the bound is 60 s.
pub(super) const ASK_BOUND: Duration = Duration::from_secs(60);

/// What a question found: its value, or why it has none. The value is reached only through a reader of both arms
/// ([`Fact::read`], [`Fact::or`]), so a caller that ignores the unknown does not compile.
#[must_use = "a fact is read with its unknown arm (read, or), never dropped"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fact<T>(Result<T, Unknown>);

impl<T> Fact<T> {
    pub fn known(value: T) -> Self {
        Self(Ok(value))
    }

    pub fn unknown(unknown: Unknown) -> Self {
        Self(Err(unknown))
    }

    /// The value through `known`, or the unknown through `unknown`.
    pub fn read<R>(self, known: impl FnOnce(T) -> R, unknown: impl FnOnce(Unknown) -> R) -> R {
        match self.0 {
            Ok(value) => known(value),
            Err(why) => unknown(why),
        }
    }

    /// The value, or what the unknown means to the caller (its stop, its failure).
    pub fn or<E>(self, unknown: impl FnOnce(Unknown) -> E) -> Result<T, E> {
        self.0.map_err(unknown)
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Fact<U> {
        Fact(self.0.map(f))
    }

    /// The fact borrowed, for a reader that does not take it.
    pub fn as_ref(&self) -> Fact<&T> {
        Fact(self.0.as_ref().map_err(Clone::clone))
    }
}

/// A question with no answer, and why: its text names the question ("`ldd --version` did not answer within 60 s").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unknown {
    asked: String,
    why: Why,
}

impl Unknown {
    pub fn new(asked: impl Into<String>, why: Why) -> Self {
        Self { asked: asked.into(), why }
    }

    #[cfg_attr(not(test), allow(dead_code, reason = "the tests read which way a question went unanswered"))]
    pub fn why(&self) -> &Why {
        &self.why
    }
}

impl std::fmt::Display for Unknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` {}", self.asked, self.why)
    }
}

/// Why a question has no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Why {
    /// The program could not be started.
    NotStarted(String),
    /// It did not finish, with all its output, within its bound; it was ended with what it started.
    Cut(Duration),
    /// A signal ended it, or the run was being stopped.
    Signal,
    /// It ended with an exit its reader does not document, or the runner lost its output or its end.
    Failed(String),
    /// It ended as documented, and what it printed does not read.
    Malformed(String),
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotStarted(e) => write!(f, "could not start ({e})"),
            Self::Cut(bound) if bound.subsec_millis() == 0 => write!(f, "did not answer within {} s", bound.as_secs()),
            Self::Cut(bound) => write!(f, "did not answer within {} ms", bound.as_millis()),
            Self::Signal => write!(f, "was ended by a signal"),
            Self::Failed(said) => write!(f, "{said}"),
            Self::Malformed(what) => write!(f, "printed {what}"),
        }
    }
}

/// What a program that ran to its own exit said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Answer {
    pub exit: i32,
    pub out: String,
    pub err: String,
}

impl Answer {
    /// The first line of stdout, trimmed.
    pub fn first_line(&self) -> String {
        self.out.lines().next().unwrap_or("").trim().to_string()
    }

    /// The last line it printed that is not blank, stderr first: a failed program says why there.
    pub fn last_said(&self) -> String {
        let last = |t: &str| t.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(String::from);
        last(&self.err).or_else(|| last(&self.out)).unwrap_or_default()
    }

    /// The answer of an exit its reader does not document.
    pub fn failed(&self) -> Why {
        Why::Failed(format!("ended with exit {}: {}", self.exit, self.last_said()))
    }

    /// `Ok` for an exit of 0, else [`Answer::failed`].
    pub fn zero(&self) -> Result<&Self, Why> {
        if self.exit == 0 { Ok(self) } else { Err(self.failed()) }
    }
}

/// Every child the install starts, and the environment it reads. [`Live`] is the computer; a test stands in for it.
pub(super) trait Machine {
    /// What the run reads of its environment (PATH, HOME, the `SOT_*` variables).
    fn env(&self) -> Env;
    /// A question within `bound`: the answer of a program that ran to its own exit, or why there is none.
    fn ask(&self, cmd: &mut Command, bound: Duration) -> Result<Answer, Why>;
    /// A step that changes something or transfers, run to its end with its output read.
    fn act(&self, cmd: &mut Command) -> Ran;
    /// A step run to its end with its output shown as it comes: its exit code.
    fn show(&self, cmd: &mut Command) -> Option<i32>;
    /// Put `dir` first on the PATH the run and its steps use.
    #[cfg_attr(windows, allow(dead_code, reason = "the Unix install puts juliaup's folder first"))]
    fn prepend_path(&self, dir: &Path);
}

/// The computer: the process environment and the run's steps.
pub(super) struct Live;

impl Machine for Live {
    fn env(&self) -> Env {
        Env::process()
    }

    fn ask(&self, cmd: &mut Command, bound: Duration) -> Result<Answer, Why> {
        cmd::output_within(cmd, bound)
    }

    fn act(&self, cmd: &mut Command) -> Ran {
        cmd::output(cmd)
    }

    fn show(&self, cmd: &mut Command) -> Option<i32> {
        cmd::shown(cmd)
    }

    fn prepend_path(&self, dir: &Path) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![dir.to_path_buf()];
        dirs.extend(std::env::split_paths(&path));
        if let Ok(joined) = std::env::join_paths(dirs) {
            std::env::set_var("PATH", joined);
        }
    }
}

/// Ask `cmd` within `bound`, its answer read by `read`, which says which exits it documents: any other is
/// [`Answer::failed`], a documented one whose text does not read is [`Why::Malformed`].
pub(super) fn ask<T>(m: &dyn Machine, cmd: &mut Command, bound: Duration, read: impl FnOnce(&Answer) -> Result<T, Why>) -> Fact<T> {
    let asked = spelled(cmd);
    match m.ask(cmd, bound).and_then(|answer| read(&answer)) {
        Ok(value) => Fact::known(value),
        Err(why) => Fact::unknown(Unknown::new(asked, why)),
    }
}

/// The question as a person reads it: the program's name and its arguments, without git's `-c` settings or a shell's
/// script text.
fn spelled(cmd: &Command) -> String {
    let program = Path::new(cmd.get_program()).file_name().map_or_else(|| cmd.get_program().to_string_lossy(), |n| n.to_string_lossy()).into_owned();
    let shell = matches!(program.as_str(), "bash" | "sh");
    let mut words = vec![program];
    let mut args = cmd.get_args().map(|a| a.to_string_lossy().into_owned());
    while let Some(a) = args.next() {
        if matches!(a.as_str(), "-c" | "-Command") {
            args.next();
            // A shell's script is followed by the name it runs as.
            if shell {
                args.next();
            }
            continue;
        }
        words.push(a);
    }
    words.join(" ")
}

/// Whether a program by this name is on the machine's PATH.
pub(super) fn on_path(m: &dyn Machine, name: &str) -> bool {
    which(m, name).is_some()
}

/// A program's path on the machine's PATH.
pub(super) fn which(m: &dyn Machine, name: &str) -> Option<std::path::PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    let env = m.env();
    std::env::split_paths(env.get("PATH")?).find_map(|d| exts.iter().map(|e| d.join(format!("{name}{e}"))).find(|p| p.is_file()))
}

#[cfg(test)]
#[path = "observe_tests.rs"]
pub(super) mod tests;
