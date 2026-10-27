use super::*;
use std::cell::RefCell;
use std::process::Command;

use super::super::cmd::tests::command;

/// How the fake computer meets one command.
#[derive(Clone)]
pub(in crate::install) enum Canned {
    /// It runs and ends with this exit, stdout and stderr.
    Says(i32, String, String),
    /// It runs the stall stand-in (the test binary itself, so on every system) through the real runner, cut at 1 s.
    Stall,
    /// It runs on this computer: Git, bash and the checkout's library, in a test's scratch folders.
    #[cfg_attr(windows, allow(dead_code, reason = "the Unix layout table runs Git live"))]
    Live,
    /// Its program is not there to start.
    #[cfg_attr(windows, allow(dead_code, reason = "the Unix gate's table removes systemctl"))]
    Missing,
    /// The first `n` commands this pattern names get the first answer, every later one the second.
    After(usize, Box<Canned>, Box<Canned>),
}

/// Answered the first `n` times by `first`, then stalled.
pub(in crate::install) fn stall_after(n: usize, first: Canned) -> Canned {
    Canned::After(n, Box::new(first), Box::new(Canned::Stall))
}

pub(in crate::install) fn says(exit: i32, out: &str) -> Canned {
    Canned::Says(exit, out.into(), String::new())
}

pub(in crate::install) fn says_err(exit: i32, err: &str) -> Canned {
    Canned::Says(exit, String::new(), err.into())
}

/// The bound a stalled question is cut at.
pub(in crate::install) const STALL_BOUND: Duration = Duration::from_secs(1);

/// A computer for a test: its environment, and an answer for each command it is asked, chosen by the last pattern that
/// is a part of the command's line (its program path and arguments). A command no pattern names panics, so no test
/// reaches the real `systemctl`, daemon or comm home.
pub(in crate::install) struct Fake {
    env: RefCell<Env>,
    answers: RefCell<Vec<(String, Canned, usize)>>,
    asked: RefCell<Vec<String>>,
    pulse: tempfile::TempDir,
}

impl Fake {
    pub fn new(env: Env) -> Self {
        Self { env: RefCell::new(env), answers: RefCell::new(Vec::new()), asked: RefCell::new(Vec::new()), pulse: tempfile::tempdir().unwrap() }
    }

    /// Answer every command whose line holds `pattern` with `canned`; a later answer wins.
    pub fn answer(&self, pattern: impl Into<String>, canned: Canned) -> &Self {
        self.answers.borrow_mut().push((pattern.into(), canned, 0));
        self
    }

    /// Set a variable of the machine's environment.
    #[cfg_attr(windows, allow(dead_code, reason = "the Unix tables set the bootstrap's variables"))]
    pub fn set_env(&self, k: &str, v: impl AsRef<std::ffi::OsStr>) -> &Self {
        self.env.borrow_mut().set(k, v);
        self
    }

    /// The lines asked or run so far, in order.
    pub fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }

    #[cfg_attr(not(windows), allow(dead_code, reason = "the Windows table asks what was asked"))]
    pub fn was_asked(&self, pattern: &str) -> bool {
        self.asked.borrow().iter().any(|l| l.contains(pattern))
    }

    fn canned(&self, cmd: &Command) -> (String, Canned) {
        let line = std::iter::once(cmd.get_program()).chain(cmd.get_args()).map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        self.asked.borrow_mut().push(line.clone());
        let mut answers = self.answers.borrow_mut();
        let Some((_, canned, seen)) = answers.iter_mut().rev().find(|(p, _, _)| line.contains(p.as_str())) else {
            panic!("the fake computer was asked `{line}`, which no answer names")
        };
        *seen += 1;
        let mut canned = canned.clone();
        while let Canned::After(n, first, then) = canned {
            canned = if *seen <= n { *first } else { *then };
        }
        (line, canned)
    }
}

impl Machine for Fake {
    fn env(&self) -> Env {
        self.env.borrow().clone()
    }

    fn ask(&self, cmd: &mut Command, bound: Duration) -> Result<Answer, Why> {
        match self.canned(cmd) {
            (_, Canned::Says(exit, out, err)) => Ok(Answer { exit, out, err }),
            (_, Canned::Stall) => cmd::output_within(&mut command("stall", &self.pulse.path().join("pulse")), bound.min(STALL_BOUND)),
            (_, Canned::Live) => cmd::output_within(cmd, bound),
            (_, Canned::Missing) => Err(Why::NotStarted("No such file or directory (os error 2)".into())),
            (_, Canned::After(..)) => unreachable!("an After is resolved to its answer"),
        }
    }

    fn act(&self, cmd: &mut Command) -> Ran {
        match self.canned(cmd) {
            (_, Canned::Says(exit, out, err)) => Ran { code: Some(exit), out, err },
            (line, Canned::Stall | Canned::Missing | Canned::After(..)) => panic!("a step that changes something is never stalled: `{line}`"),
            (_, Canned::Live) => cmd::output(cmd),
        }
    }

    fn show(&self, cmd: &mut Command) -> Option<i32> {
        match self.canned(cmd) {
            (_, Canned::Says(exit, ..)) => Some(exit),
            (line, Canned::Stall | Canned::Missing | Canned::After(..)) => panic!("a step that changes something is never stalled: `{line}`"),
            (_, Canned::Live) => cmd::shown(cmd),
        }
    }

    fn prepend_path(&self, dir: &Path) {
        let mut env = self.env.borrow_mut();
        let mut dirs = vec![dir.to_path_buf()];
        dirs.extend(env.get("PATH").map(|p| std::env::split_paths(p).collect::<Vec<_>>()).unwrap_or_default());
        env.set("PATH", std::env::join_paths(dirs).unwrap());
    }
}

/// The names and bytes under `dir`, sorted, to show a stop wrote nothing.
pub(in crate::install) fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            let bytes = if p.is_file() { std::fs::read(&p).unwrap_or_default() } else { Vec::new() };
            out.push((p.to_string_lossy().into_owned(), bytes));
            if p.is_dir() {
                walk(&p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// Each way a question goes unanswered comes back as its own reason, from a real child through the real runner: one
/// cut at its bound (its partial output discarded), one that cannot start, an exit its reader does not document, and a
/// documented exit whose text does not read. A known answer is read only through both arms.
#[test]
fn a_question_is_answered_or_says_why_not() {
    let fake = Fake::new(Env::empty());
    fake.answer("stalls", Canned::Stall).answer("fails", says_err(1, "Failed to connect to bus")).answer("garbles", says(0, "???")).answer("answers", says(0, "42\n"));
    let read = |a: &Answer| a.zero()?.first_line().parse::<u32>().map_err(|_| Why::Malformed(format!("no number ('{}')", a.first_line())));
    let start = std::time::Instant::now();
    let cut = ask(&fake, &mut Command::new("stalls"), ASK_BOUND, read).read(|_| panic!("a cut question answered"), |u| u);
    assert_eq!(cut.why(), &Why::Cut(STALL_BOUND));
    assert_eq!(cut.to_string(), "`stalls` did not answer within 1 s");
    assert!(start.elapsed() < Duration::from_secs(8), "the cut took {:?}", start.elapsed());
    let failed = ask(&fake, &mut Command::new("fails"), ASK_BOUND, read).read(|_| panic!(), |u| u);
    assert_eq!(failed.to_string(), "`fails` ended with exit 1: Failed to connect to bus");
    let malformed = ask(&fake, &mut Command::new("garbles"), ASK_BOUND, read).read(|_| panic!(), |u| u);
    assert_eq!(malformed.why(), &Why::Malformed("no number ('???')".into()));
    assert_eq!(ask(&fake, &mut Command::new("answers"), ASK_BOUND, read).or(|_| ()), Ok(42));

    let missing = ask(&Live, &mut Command::new("sot-no-such-program-anywhere"), ASK_BOUND, |_| Ok(())).read(|()| panic!("a program that did not start answered"), |u| u);
    assert!(matches!(missing.why(), Why::NotStarted(_)), "{missing}");

    // The question as a person reads it: no git `-c` settings, no shell script text.
    let mut git = cmd::git_in(Path::new("/r"));
    git.args(["rev-parse", "HEAD"]);
    assert_eq!(spelled(&git), format!("{} -C /r rev-parse HEAD", if cfg!(windows) { "git.exe" } else { "git" }));
    assert_eq!(spelled(Command::new("/x/bash").args(["-c", ". lib", "bash", "/c/lib.sh", "sot_boot_wired", "/u"])), "bash /c/lib.sh sot_boot_wired /u");
}

/// A program a signal ended has no answer, whatever it printed first.
#[cfg(unix)]
#[test]
fn a_signal_is_no_answer() {
    let d = tempfile::tempdir().unwrap();
    let script = d.path().join("killed");
    sot_platform::test_exec::write_executable(&script, "#!/bin/sh\necho 'sotd 0.7.0 (0123456789abc protocol 9)'\nkill -9 $$\n");
    let why = ask(&Live, &mut Command::new(&script), Duration::from_secs(20), |a| Ok(a.first_line())).read(|line| panic!("a killed program answered '{line}'"), |u| u);
    assert_eq!(why.why(), &Why::Signal);
}
