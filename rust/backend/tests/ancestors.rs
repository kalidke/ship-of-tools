#![cfg(windows)]
//! `sotd ancestors` — a real `sotd` binary run as a subprocess, no daemon. It
//! prints this process's ancestors, parent first, one `<exe>\t<command line>`
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

/// One `(exe, command line)` pair per line; a missing tab leaves the command line empty.
fn lines_of(out: &std::process::Output) -> Vec<(String, String)> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            let (exe, cl) = l.split_once('\t').unwrap_or((l, ""));
            (exe.trim().to_string(), cl.to_string())
        })
        .collect()
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
    assert!(
        lines[0].0.eq_ignore_ascii_case(&own_name()),
        "line 1 is {:?}, want this test's {:?}; all: {lines:?}",
        lines[0].0,
        own_name()
    );
    assert!(
        lines[0].1.replace('\\', "/").to_lowercase().contains(&own_path()),
        "line 1's command line {:?} does not hold this test's path {:?}",
        lines[0].1,
        own_path()
    );
}

#[test]
fn the_chain_survives_an_msys_shell() {
    // Forward slashes: the path reaches `Command` and the shell untouched.
    let bash = "C:/Program Files/Git/bin/bash.exe";
    assert!(std::path::Path::new(bash).exists(), "Git Bash is missing at {bash}");
    // `; true` keeps bash from exec-replacing itself with sotd, which would
    // leave no shell in the chain.
    let out = Command::new(bash)
        .args(["-c", "\"$SOTD\" ancestors; true"])
        .env("SOTD", sotd_exe().to_string_lossy().replace('\\', "/"))
        .output()
        .expect("run bash");
    let lines = lines_of(&out);
    let at = lines
        .iter()
        .position(|l| l.0.eq_ignore_ascii_case("bash.exe"))
        .unwrap_or_else(|| panic!("no bash.exe line in {lines:?}; stderr: {}", String::from_utf8_lossy(&out.stderr)));
    // Git's bash.exe is a wrapper around usr/bin/bash.exe, and MSYS adds fork
    // stubs: more than one bash.exe, and anything in between, is fine.
    let own = lines[at..].iter().find(|l| l.0.eq_ignore_ascii_case(&own_name()));
    let own = own.unwrap_or_else(|| panic!("this test's {:?} does not follow bash.exe in {lines:?}", own_name()));
    assert!(
        own.1.replace('\\', "/").to_lowercase().contains(&own_path()),
        "the command line {:?} of this test's line does not hold its path {:?}",
        own.1,
        own_path()
    );
}
