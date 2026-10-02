#![cfg(windows)]
//! `sotd ancestors` — a real `sotd` binary run as a subprocess, no daemon. It
//! prints this process's ancestors, parent first, one `<pid>\t<exe>\t<command line>`
//! line each, so comm-lib.sh can count the agents above a comm script on a host
//! where `ps` cannot see them.
//!
//! Two proofs: run directly, the first line is this test's own executable and
//! its command line holds the test's own path; run through Git Bash (an MSYS
//! shell, perhaps two, stands between them), a `bash.exe` line appears and this
//! test's executable follows it somewhere. A missing Git Bash FAILS:
//! the second proof is the one that decides whether the chain survives an MSYS
//! exec stub, and CI's windows-latest has Git Bash.

use std::path::PathBuf;
use std::process::Command;

const GIT_BASH: &str = "C:/Program Files/Git/bin/bash.exe";

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}

fn own_name() -> String {
    std::env::current_exe()
        .expect("current_exe")
        .file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned()
}

/// One `(pid, exe, command line)` per line of TEXT; a line whose first field is
/// not a pid fails the test.
fn parse_lines(text: &str) -> Vec<(u32, String, String)> {
    text.lines()
        .map(|l| {
            let (pid, rest) = l.split_once('\t').unwrap_or_else(|| panic!("no pid column in {l:?}"));
            let pid = pid.parse().unwrap_or_else(|_| panic!("{pid:?} is not a pid, in {l:?}"));
            let (exe, cl) = rest.split_once('\t').unwrap_or((rest, ""));
            (pid, exe.trim().to_string(), cl.to_string())
        })
        .collect()
}

fn lines_of(out: &std::process::Output) -> Vec<(u32, String, String)> {
    parse_lines(&String::from_utf8_lossy(&out.stdout))
}

/// Whether LINES hold this test's own executable, its path in the command line.
fn holds_this_test(lines: &[(u32, String, String)]) -> bool {
    lines.iter().any(|l| {
        l.1.eq_ignore_ascii_case(&own_name()) && l.2.replace('\\', "/").to_lowercase().contains(&own_path())
    })
}

/// The test executable's own path, forward slashes and lower case, for comparing
/// against a command line that may spell it either way.
fn own_path() -> String {
    std::env::current_exe().expect("current_exe").to_string_lossy().replace('\\', "/").to_lowercase()
}

#[test]
fn first_line_is_the_direct_parent() {
    let out = Command::new(sotd_exe()).arg("ancestors").output().expect("run sotd");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let lines = lines_of(&out);
    assert!(!lines.is_empty(), "no ancestors printed");
    assert_eq!(lines[0].0, std::process::id(), "line 1's pid is not this test's: {lines:?}");
    assert!(
        lines[0].1.eq_ignore_ascii_case(&own_name()),
        "line 1 is {:?}, want this test's {:?}; all: {lines:?}",
        lines[0].1,
        own_name()
    );
    assert!(
        lines[0].2.replace('\\', "/").to_lowercase().contains(&own_path()),
        "line 1's command line {:?} does not hold this test's path {:?}",
        lines[0].2,
        own_path()
    );
}

#[test]
fn the_chain_survives_an_msys_shell() {
    assert!(std::path::Path::new(GIT_BASH).exists(), "Git Bash is missing at {GIT_BASH}");
    // `; true` keeps bash from exec-replacing itself with sotd, which would
    // leave no shell in the chain.
    let out = Command::new(GIT_BASH)
        .args(["-c", "\"$SOTD\" ancestors; true"])
        .env("SOTD", sotd_exe().to_string_lossy().replace('\\', "/"))
        .output()
        .expect("run bash");
    let lines = lines_of(&out);
    let at = lines
        .iter()
        .position(|l| l.1.eq_ignore_ascii_case("bash.exe"))
        .unwrap_or_else(|| panic!("no bash.exe line in {lines:?}; stderr: {}", String::from_utf8_lossy(&out.stderr)));
    // Git's bash.exe is a wrapper around usr/bin/bash.exe, and MSYS adds fork
    // stubs: more than one bash.exe, and anything in between, is fine.
    let own = lines[at..].iter().find(|l| l.1.eq_ignore_ascii_case(&own_name()));
    let own = own.unwrap_or_else(|| panic!("this test's {:?} does not follow bash.exe in {lines:?}", own_name()));
    assert!(
        own.2.replace('\\', "/").to_lowercase().contains(&own_path()),
        "the command line {:?} of this test's line does not hold its path {:?}",
        own.2,
        own_path()
    );
}

#[test]
fn from_starts_above_the_named_process() {
    let own = std::process::id();
    let all = Command::new(sotd_exe()).arg("ancestors").output().expect("run sotd");
    let from = Command::new(sotd_exe()).args(["ancestors", "--from", &own.to_string()]).output().expect("run sotd");
    assert!(all.status.success(), "stderr: {}", String::from_utf8_lossy(&all.stderr));
    assert!(from.status.success(), "stderr: {}", String::from_utf8_lossy(&from.stderr));
    let (all, from) = (lines_of(&all), lines_of(&from));
    assert_eq!(all.first().map(|l| l.0), Some(own), "the default walk starts at this test: {all:?}");
    assert_eq!(from, all[1..].to_vec(), "--from {own} is the default walk without its first line");
}

#[test]
fn other_arguments_are_a_usage_error() {
    for args in [&["ancestors", "--from"][..], &["ancestors", "--from", "x"], &["ancestors", "extra"]] {
        let out = Command::new(sotd_exe()).args(args).output().expect("run sotd");
        assert_eq!(out.status.code(), Some(2), "{args:?}: stdout {:?}", String::from_utf8_lossy(&out.stdout));
    }
}

#[test]
fn the_cygwin_parent_carries_the_walk_past_an_msys_exec() {
    assert!(std::path::Path::new(GIT_BASH).exists(), "Git Bash is missing at {GIT_BASH}");
    // The outer shell's parent is Git's native launcher, so its Cygwin ppid is 1.
    // The inner shell is an MSYS exec of the outer's fork, so its own Windows
    // parent has exited; it reads its Cygwin parent's Windows pid and walks from there.
    let inner = r#"read -r _ _ _ pp _ < /proc/$$/stat; read -r _ _ _ opp _ < /proc/$pp/stat; read -r w < /proc/$pp/winpid; echo "outer-ppid=$opp"; "$SOTD" ancestors --from "$w""#;
    // `; true` keeps the outer shell from exec-replacing itself with the inner one.
    let out = Command::new(GIT_BASH)
        .args(["-c", r#"bash -c "$INNER"; true"#])
        .env("INNER", inner)
        .env("SOTD", sotd_exe().to_string_lossy().replace('\\', "/"))
        .output()
        .expect("run bash");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (head, walk) = text.split_once('\n').unwrap_or((text.as_str(), ""));
    assert_eq!(head.trim(), "outer-ppid=1", "stdout {text:?}; stderr: {}", String::from_utf8_lossy(&out.stderr));
    let walk = parse_lines(walk);
    assert!(holds_this_test(&walk), "this test's {:?} is not above the outer shell: {walk:?}", own_name());
}

#[test]
fn an_exec_under_a_native_parent_keeps_the_chain() {
    assert!(std::path::Path::new(GIT_BASH).exists(), "Git Bash is missing at {GIT_BASH}");
    // The shell Git's native launcher starts execs another MSYS shell: Cygwin keeps
    // the old Windows process as the new one's parent while a native parent waits.
    let inner = r#"read -r w < /proc/$$/winpid; "$SOTD" ancestors --from "$w""#;
    let out = Command::new(GIT_BASH)
        .args(["-c", r#"exec bash -c "$INNER""#])
        .env("INNER", inner)
        .env("SOTD", sotd_exe().to_string_lossy().replace('\\', "/"))
        .output()
        .expect("run bash");
    let walk = lines_of(&out);
    assert!(
        holds_this_test(&walk),
        "this test's {:?} is not above the exec'd shell: {walk:?}; stderr: {}",
        own_name(),
        String::from_utf8_lossy(&out.stderr)
    );
}
