//! The preview tools: `pdftoppm` and `pdfinfo` (PDF pages, poppler) and `ffmpeg` (a video's poster frame), which the
//! preview plugins call on the daemon's computer. The install looks for them where the daemon will, and offers the
//! platform's package command for the missing ones at a console (scripts/CLAUDE.md, Installers, "The preview tools").

use std::ffi::OsStr;
use std::process::Command;
#[cfg(unix)]
use std::time::Duration;

use super::{cmd, daemon, verdict};

/// A tool a preview plugin calls, what it is for, and the package that provides it on each package manager.
pub(super) struct Tool {
    pub name: &'static str,
    pub what: &'static str,
    pub apt: &'static str,
    pub brew: &'static str,
    pub winget: &'static str,
}

pub(super) const TOOLS: [Tool; 3] = [
    Tool { name: "pdftoppm", what: "PDF pages", apt: "poppler-utils", brew: "poppler", winget: "oschwartz10612.Poppler" },
    Tool { name: "pdfinfo", what: "PDF page counts", apt: "poppler-utils", brew: "poppler", winget: "oschwartz10612.Poppler" },
    Tool { name: "ffmpeg", what: "video poster frames", apt: "ffmpeg", brew: "ffmpeg", winget: "Gyan.FFmpeg" },
];

/// The tools not found in any folder of `path`.
pub(super) fn missing(path: &OsStr) -> Vec<&'static Tool> {
    TOOLS.iter().filter(|t| !cmd::on_path_in(path, t.name)).collect()
}

/// What an offer does outside itself: ask the console (`None`: no person is there), run a package command (its exit
/// code, `None` when it did not start or a signal ended it), say one line, and tell whether a program is on PATH.
pub(super) struct Seams<'a> {
    pub ask: &'a dyn Fn(&str) -> Option<String>,
    pub run: &'a dyn Fn(&[String]) -> Option<i32>,
    pub say: &'a dyn Fn(&str),
    pub has: &'a dyn Fn(&str) -> bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Manager {
    Apt,
    Brew,
    Winget,
}

fn manager(system: &str, has: &dyn Fn(&str) -> bool) -> Option<Manager> {
    match system {
        "linux" if has("apt-get") => Some(Manager::Apt),
        "macos" if has("brew") => Some(Manager::Brew),
        "windows" if has("winget") => Some(Manager::Winget),
        _ => None,
    }
}

/// The package commands that install `left`, one per program run: apt and brew take every package, winget one.
fn commands(m: Manager, left: &[&Tool]) -> Vec<Vec<String>> {
    let mut pkgs: Vec<&str> = Vec::new();
    for t in left {
        let p = match m {
            Manager::Apt => t.apt,
            Manager::Brew => t.brew,
            Manager::Winget => t.winget,
        };
        if !pkgs.contains(&p) {
            pkgs.push(p);
        }
    }
    let (head, tail): (&[&str], &[&str]) = match m {
        Manager::Apt => (&["sudo", "apt-get", "install"], &[]),
        Manager::Brew => (&["brew", "install"], &[]),
        Manager::Winget => (&["winget", "install", "--id"], &["-e"]),
    };
    let make = |ps: &[&str]| head.iter().chain(ps).chain(tail).map(|s| (*s).to_string()).collect::<Vec<String>>();
    if m == Manager::Winget { pkgs.iter().map(|p| make(&[p])).collect() } else { vec![make(&pkgs)] }
}

/// Where a person gets the tools when this computer has no package manager the installer knows.
fn hint(system: &str) -> &'static str {
    match system {
        "windows" => "winget is not on this computer: download poppler (https://github.com/oschwartz10612/poppler-windows/releases) and ffmpeg (https://www.gyan.dev/ffmpeg/builds/) and put their bin folders on PATH",
        "macos" => "install Homebrew (https://brew.sh), then brew install poppler ffmpeg",
        _ => "install poppler-utils and ffmpeg with this computer's package manager",
    }
}

/// Look for the tools in `path`, the daemon's search path, and offer to install the missing ones. `system` is `linux`,
/// `macos` or `windows`. Changes nothing the verdict reads: it returns nothing and says one line of its own.
pub(super) fn offer(system: &str, path: &OsStr, s: &Seams) {
    let left = missing(path);
    if left.is_empty() {
        return;
    }
    let list = left.iter().map(|t| format!("{} ({})", t.name, t.what)).collect::<Vec<_>>().join(", ");
    let Some(m) = manager(system, s.has) else {
        return (s.say)(&format!("preview tools missing: {list}; {}", hint(system)));
    };
    let cmds = commands(m, &left);
    let line = cmds.iter().map(|c| c.join(" ")).collect::<Vec<_>>().join("; ");
    let Some(answer) = (s.ask)(&format!("Preview tools missing on this computer: {list}. Install them with `{line}`? [y/N] ")) else {
        return (s.say)(&format!("preview tools missing: {list}; install them with: {line}"));
    };
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return (s.say)(&format!("preview tools not installed; to install them: {line}"));
    }
    for c in &cmds {
        let why = match (s.run)(c) {
            Some(0) => continue,
            Some(n) => format!("failed (exit {n})"),
            None => "did not finish".to_string(),
        };
        return (s.say)(&format!("preview tools: `{}` {why}; run it yourself to install them", c.join(" ")));
    }
    let still = missing(path);
    (s.say)(&if still.is_empty() {
        "preview tools installed".to_string()
    } else {
        let names = still.iter().map(|t| t.name).collect::<Vec<_>>().join(", ");
        format!("preview tools: the package command finished, but {names} still not found on the daemon's search path (a daemon or shell started later searches a changed one)")
    });
}

/// [`offer`] with the real console, package command and PATH.
pub(super) fn offer_here(system: &str, path: &OsStr, say: &dyn Fn(&str)) {
    let run = |argv: &[String]| {
        verdict::step("tools");
        cmd::shown_on(Command::new(&argv[0]).args(&argv[1..]), daemon::console_stdin())
    };
    offer(system, path, &Seams { ask: &daemon::read_answer, run: &run, say, has: &cmd::on_path });
}

/// The search path a managed unit's daemon runs with: the user manager's environment (`systemctl --user
/// show-environment`, as `manager_env`) with the `~/.bashrc` exports the unit's `bash -c` adds. `None` when bash
/// cannot say. The unit runs `/bin/bash`, so this does. A `PATH=` in the unit's own `Environment=` is not read.
#[cfg(unix)]
pub(super) fn unit_path(home: &std::path::Path, manager_env: &str) -> Option<std::ffi::OsString> {
    let vars = manager_env.lines().filter_map(|l| l.split_once('=')).filter(|(k, v)| !k.is_empty() && !v.starts_with('$'));
    let mut bash = Command::new("/bin/bash");
    bash.env_clear().envs(vars).env("HOME", home);
    bash.args(["-c", r#"[ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"; printf '\nSOT_PATH=%s\n' "$PATH""#]);
    let ran = cmd::output_within(&mut bash, Duration::from_secs(30));
    ran.out.lines().rev().find_map(|l| l.strip_prefix("SOT_PATH=")).map(Into::into)
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
