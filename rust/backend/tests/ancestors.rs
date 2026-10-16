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
//! exec stub, and CI's windows-latest has Git Bash. The line comm-probe.sh types
//! into a cmd.exe row starts a script whose walk passes cmd.exe; a script given
//! alone to `bash -c` is exec'd twice and its walk ends at once (PROTOCOL.md's
//! row rule states that limit).

use std::process::Command;
use std::time::Duration;

use sot_log::test_wait::{run_within, Ran};

#[path = "support/sotd.rs"]
mod sotd;

const GIT_BASH: &str = "C:/Program Files/Git/bin/bash.exe";

const BOUND: Duration = Duration::from_secs(30);

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

fn lines_of(ran: &Ran) -> Vec<(u32, String, String)> {
    parse_lines(&ran.stdout)
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
    let ran = run_within(sotd::sotd_command().arg("ancestors"), BOUND).ok();
    let lines = lines_of(&ran);
    assert!(!lines.is_empty(), "no ancestors printed: {ran}");
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
    let ran = run_within(
        Command::new(GIT_BASH)
            .args(["-c", "\"$SOTD\" ancestors; true"])
            .env("SOTD", sotd::sotd_program().to_string_lossy().replace('\\', "/")),
        BOUND,
    )
    .ok();
    let lines = lines_of(&ran);
    let at = lines
        .iter()
        .position(|l| l.1.eq_ignore_ascii_case("bash.exe"))
        .unwrap_or_else(|| panic!("no bash.exe line in {lines:?}: {ran}"));
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
    let all = run_within(sotd::sotd_command().arg("ancestors"), BOUND).ok();
    let from = run_within(sotd::sotd_command().args(["ancestors", "--from", &own.to_string()]), BOUND).ok();
    let (all, from) = (lines_of(&all), lines_of(&from));
    assert_eq!(all.first().map(|l| l.0), Some(own), "the default walk starts at this test: {all:?}");
    assert_eq!(from, all[1..].to_vec(), "--from {own} is the default walk without its first line");
}

#[test]
fn other_arguments_are_a_usage_error() {
    for args in [&["ancestors", "--from"][..], &["ancestors", "--from", "x"], &["ancestors", "extra"]] {
        let ran = run_within(sotd::sotd_command().args(args), BOUND);
        assert_eq!(ran.status.code(), Some(2), "{args:?}: {ran}");
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
    let ran = run_within(
        Command::new(GIT_BASH)
            .args(["-c", r#"bash -c "$INNER"; true"#])
            .env("INNER", inner)
            .env("SOTD", sotd::sotd_program().to_string_lossy().replace('\\', "/")),
        BOUND,
    );
    let (head, walk) = ran.stdout.split_once('\n').unwrap_or((ran.stdout.as_str(), ""));
    assert_eq!(head.trim(), "outer-ppid=1", "{ran}");
    let walk = parse_lines(walk);
    assert!(holds_this_test(&walk), "this test's {:?} is not above the outer shell: {walk:?}", own_name());
}

#[test]
fn an_exec_under_a_native_parent_keeps_the_chain() {
    assert!(std::path::Path::new(GIT_BASH).exists(), "Git Bash is missing at {GIT_BASH}");
    // The shell Git's native launcher starts execs another MSYS shell: Cygwin keeps
    // the old Windows process as the new one's parent while a native parent waits.
    let inner = r#"read -r w < /proc/$$/winpid; "$SOTD" ancestors --from "$w""#;
    let ran = run_within(
        Command::new(GIT_BASH)
            .args(["-c", r#"exec bash -c "$INNER""#])
            .env("INNER", inner)
            .env("SOTD", sotd::sotd_program().to_string_lossy().replace('\\', "/")),
        BOUND,
    );
    let walk = lines_of(&ran);
    assert!(holds_this_test(&walk), "this test's {:?} is not above the exec'd shell: {walk:?}: {ran}", own_name());
}

/// A stand-in for comm-probe.sh: its Cygwin parent, then its native walk. Its `#!` line is what makes `bash -c` exec
/// it through env, as it does the real script.
const WALK_SCRIPT: &str = "#!/usr/bin/env bash\nread -r _ _ _ pp _ < /proc/$$/stat; read -r w < /proc/$$/winpid\necho \"ppid=$pp\"; \"$SOTD\" ancestors --from \"$w\"\n";

/// LINE run as a cmd.exe row runs a typed line (`cmd /s /c` takes it as written): the stand-in's parent line and its
/// walk.
fn run_in_cmd(line: &str) -> (String, Vec<(u32, String, String)>) {
    use std::os::windows::process::CommandExt;
    let ran = run_within(
        Command::new("cmd.exe")
            .raw_arg(format!("/d /s /c \"{line}\""))
            .env("SOTD", sotd::sotd_program().to_string_lossy().replace('\\', "/")),
        BOUND,
    )
    .ok();
    let (head, walk) = ran.stdout.split_once('\n').unwrap_or((ran.stdout.as_str(), ""));
    (head.trim().to_string(), parse_lines(walk))
}

/// comm-probe.sh types its start line into a `pipe:` daemon's probe row, whose shell is cmd.exe. Run by cmd.exe here,
/// the script it starts walks through cmd.exe to this test, so in a row it reaches the row's capsule. Given alone to
/// `bash -c`, the same script is exec'd twice (bash execs it, and its `#!/usr/bin/env bash` execs bash), the Windows
/// parent of its image has exited, and its walk ends at once: the limit PROTOCOL.md states.
#[test]
fn the_probe_start_line_keeps_the_chain_a_lone_dash_c_loses_it() {
    assert!(std::path::Path::new(GIT_BASH).exists(), "Git Bash is missing at {GIT_BASH}");
    let dir = tempfile::tempdir().expect("tempdir");
    let fwd = |p: &std::path::Path| p.to_string_lossy().replace('\\', "/");
    let (bin, stand_in) = (dir.path().join("bin"), dir.path().join("probe"));
    let stage = format!("{}/../../comm/tests/stage-bin.sh", env!("CARGO_MANIFEST_DIR")).replace('\\', "/");
    run_within(Command::new(GIT_BASH).arg(stage).arg(fwd(&bin)), BOUND).ok();
    std::fs::create_dir_all(&stand_in).expect("mkdir the stand-in's folder");
    std::fs::write(stand_in.join("comm-probe.sh"), WALK_SCRIPT).expect("write the stand-in");
    // The line `up` types, from the staged comm-probe.sh with its folder pointed at the stand-in's.
    let ran = run_within(
        Command::new(GIT_BASH)
            .args(["-c", r#". "$1"; SCRIPT_DIR="$2"; ENDPOINT='pipe:\\.\pipe\probe-test'; probe_start_line"#, "_"])
            .arg(fwd(&bin.join("comm-probe.sh")))
            .arg(fwd(&stand_in))
            .env("SOT_COMM_HOME", fwd(&dir.path().join("home"))),
        BOUND,
    );
    let line = ran.stdout.trim().to_string();
    let launcher = line.strip_prefix('"').and_then(|l| l.split_once('"')).map(|(l, _)| l.to_string());
    let launcher = launcher.unwrap_or_else(|| panic!("no quoted launcher opens the start line: {ran}"));
    assert!(!line.contains(" -c "), "the start line gives the script to `bash -c`: {line}");

    let (parent, walk) = run_in_cmd(&line);
    assert_eq!(parent, "ppid=1", "the started script's parent is not the native launcher: {walk:?}");
    assert!(walk.iter().any(|l| l.1.eq_ignore_ascii_case("cmd.exe")), "the walk misses cmd.exe: {walk:?}");
    assert!(holds_this_test(&walk), "the walk misses this test: {walk:?}");

    let script = fwd(&stand_in.join("comm-probe.sh"));
    let (parent, walk) = run_in_cmd(&format!("\"{launcher}\" -c \"{script}\""));
    assert_eq!(parent, "ppid=1", "{walk:?}");
    assert!(walk.is_empty(), "a script exec'd twice under a native parent walks on, so PROTOCOL.md's limit is gone: {walk:?}");
}
