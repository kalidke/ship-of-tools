#![cfg(unix)]
//! `sotd agent-exec` (ADR 0046 decision 4) — a real `sotd` binary, run
//! directly as a subprocess. No daemon, no socket: `agent-exec` is a
//! pure-subcommand arm answered before any of that starts (`main.rs`),
//! so this suite is a plain `Command::output()` proof, not a wire-
//! protocol one (contrast `tests/capsule_workspaces.rs`).
//!
//! The fake `claude` is a printing stub (a shell script), not the
//! unlaunchable one `tests/support/mod.rs::seed_fake_unlaunchable_claude`
//! seeds for OTHER suites — this one needs real, inspectable output:
//! its own resolved argv (so `--continue` is provably absent and the
//! given flags land in order, before the bootstrap skill) and a nesting
//! env var (so scrubbing is provably real, not merely undocumented).

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}

/// A `claude` stub at `<home>/.local/bin/claude` — found ONLY via
/// `resolve_claude`'s HOME-derived fallback (the proof's own PATH never
/// contains it), matching CLAUDE.md's documented gotcha this decision
/// exists to fix: a daemon-spawned process's PATH lacks `~/.local/bin`.
/// Prints its own resolved argv (one `ARGV<n>=` line each, `$0` first)
/// and the env vars a real launch cares about, so a single subprocess
/// run proves resolution, scrubbing, and the PATH prepend at once.
fn seed_printing_claude_stub(home: &std::path::Path) -> PathBuf {
    let dir = home.join(".local").join("bin");
    std::fs::create_dir_all(&dir).expect("mkdir ~/.local/bin");
    let claude = dir.join("claude");
    std::fs::write(
        &claude,
        b"#!/bin/sh\n\
          echo \"ARGV0=$0\"\n\
          i=1\n\
          for a in \"$@\"; do\n\
          echo \"ARG$i=$a\"\n\
          i=$((i + 1))\n\
          done\n\
          echo \"CLAUDECODE=${CLAUDECODE:-<unset>}\"\n\
          echo \"PATH=$PATH\"\n",
    )
    .expect("write fake claude stub");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake claude stub");
    claude
}

/// `sotd agent-exec claude --x` from a shell whose `PATH` lacks
/// `~/.local/bin` and whose env has `CLAUDECODE=1` — the exact scenario
/// a daemon started from within a claude session (or a tmux server
/// first started by one) leaves an `sotd agent-exec` caller in. Proves,
/// in one real subprocess: resolution finds the stub via the HOME
/// fallback (never on the literal PATH given), the nesting env is
/// scrubbed before exec, `~/.local/bin` lands on the exec'd process's
/// own `PATH`, and the argv shape is `[claude, --permission-mode, auto,
/// --x, /sot-session-start]` — no `--continue` (`agent-exec` never adds
/// it; that stays the daemon's own default for a capsule row).
#[test]
fn agent_exec_claude_resolves_scrubs_and_execs_with_no_continue() {
    let home = tempfile::tempdir().expect("tempdir");
    let claude = seed_printing_claude_stub(home.path());

    let out = Command::new(sotd_exe())
        .arg("agent-exec")
        .arg("claude")
        .arg("--x")
        .env_clear()
        .env("HOME", home.path())
        // Deliberately WITHOUT ~/.local/bin -- the whole point of the
        // fallback this proof exercises.
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .output()
        .expect("spawn sotd agent-exec");

    assert!(
        out.status.success(),
        "sotd agent-exec exited {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    let argv0 = format!("ARGV0={}", claude.display());
    assert!(
        stdout.lines().any(|l| l == argv0),
        "expected {argv0:?} in stdout:\n{stdout}"
    );
    let expected_args = ["--permission-mode", "auto", "--x", "/sot-session-start"];
    for (i, want) in expected_args.iter().enumerate() {
        let line = format!("ARG{}={want}", i + 1);
        assert!(
            stdout.lines().any(|l| l == line),
            "expected {line:?} in stdout:\n{stdout}"
        );
    }
    assert!(
        !stdout.lines().any(|l| l.starts_with("ARG") && l.ends_with("=--continue")),
        "agent-exec must never add --continue itself:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|l| l == "CLAUDECODE=<unset>"),
        "nesting env must be scrubbed before exec:\n{stdout}"
    );
    let local_bin = home.path().join(".local").join("bin");
    let path_line = stdout
        .lines()
        .find(|l| l.starts_with("PATH="))
        .unwrap_or_default();
    assert!(
        path_line.starts_with(&format!("PATH={}:", local_bin.display())),
        "expected ~/.local/bin prepended to PATH, got: {path_line}"
    );
}

/// An unknown kind refuses with `agent_argv`'s own vocabulary and exits
/// 2 -- never a silent bare-shell substitution for a kind the caller
/// explicitly asked for.
#[test]
fn agent_exec_unknown_kind_exits_2() {
    let out = Command::new(sotd_exe())
        .arg("agent-exec")
        .arg("bogus")
        .env_clear()
        .output()
        .expect("spawn sotd agent-exec");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("bogus"), "got: {stderr}");
}
