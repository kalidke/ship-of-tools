#![cfg(windows)]
//! `sotd ancestors` — a real `sotd` binary run as a subprocess, no daemon. It
//! prints this process's ancestor executables, parent first, so comm-lib.sh can
//! count the agents above a comm script on a host where `ps` cannot see them.
//!
//! Two proofs: run directly, the first line is this test's own executable; run
//! through Git Bash (an MSYS shell stands between them), a `bash.exe` line
//! appears and this test's executable follows it. A missing Git Bash FAILS:
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

fn lines_of(out: &std::process::Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).collect()
}

#[test]
fn first_line_is_the_direct_parent() {
    let out = Command::new(sotd_exe()).arg("ancestors").output().expect("run sotd");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let lines = lines_of(&out);
    assert!(!lines.is_empty(), "no ancestors printed");
    assert!(
        lines[0].eq_ignore_ascii_case(&own_name()),
        "line 1 is {:?}, want this test's {:?}; all: {lines:?}",
        lines[0],
        own_name()
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
        .position(|l| l.eq_ignore_ascii_case("bash.exe"))
        .unwrap_or_else(|| panic!("no bash.exe line in {lines:?}; stderr: {}", String::from_utf8_lossy(&out.stderr)));
    assert!(
        lines.get(at + 1).is_some_and(|l| l.eq_ignore_ascii_case(&own_name())),
        "this test's {:?} does not follow bash.exe in {lines:?}",
        own_name()
    );
}
