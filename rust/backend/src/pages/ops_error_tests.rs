//! Failed-render replies exercised through the real handler with owned Unix stand-ins.

use super::{handle_quarto_open, Session};
use crate::paths::EnvGuard;
use sot_log::test_isolated::{self, ChildWaitKind, FixtureOutcome, OutputFailure};
use std::io::Write;
use std::process::Stdio;
use std::time::{Duration, Instant};

const REQUEST_ID: u64 = 407;
const TRANSPORT_ERROR: &str = "Read invalid transport file that did not end with a newline";
const UPGRADE: &str = "Quarto before 1.9.38 can read the Julia server's transport file before its line is written. Upgrade Quarto to 1.9.38 or later.";
const LOCK_ACTOR: &str = "SOT_TEST_QUARTO_ERROR_LOCK_ACTOR";
const LOCK_READY: &str = "quarto-error-lock-holder-ready";
const LOCK_STDERR: &str = "quarto-error-lock-holder-stderr";
const CHILD: &str = "SOT_TEST_QUARTO_ERROR_CHILD";

fn isolated(name: &str) -> bool {
    if std::env::var(CHILD).as_deref() == Ok(name) {
        test_isolated::enter(name);
        return true;
    }
    let outcome = supervise(name, CHILD, test_isolated::ISOLATION_TIMEOUT);
    require_observations(&outcome);
    assert!(outcome.wait.as_ref().is_ok_and(|status| status.success()),
        "Quarto error fixture did not succeed: {}", failure_details(&outcome));
    false
}

fn supervise(name: &str, marker: &str, bound: Duration) -> FixtureOutcome<()> {
    let deadline = Instant::now() + bound;
    let (mut command, entry) = test_isolated::test_command(name);
    command.env(marker, name).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[allow(clippy::disallowed_methods, reason = "test-only exact-body child immediately transferred to bounded test_isolated supervision")]
    let child = command.spawn().expect("spawn the exact Quarto error test body");
    let outcome = test_isolated::supervise_fixture_until(child, entry, deadline, |_, _, _| Ok(()));
    outcome.report(name);
    eprintln!("quarto-error-fixture status test={name} wait={:?}", outcome.wait);
    report_stream(name, "stdout", &outcome.streams.stdout);
    report_stream(name, "stderr", &outcome.streams.stderr);
    outcome
}

fn report_stream(name: &str, stream: &str, result: &Result<String, OutputFailure>) {
    eprintln!("--- quarto-error {stream} begin test={name} ---");
    match result {
        Ok(text) => eprintln!("{text}"),
        Err(error) => eprintln!("{}\n[read error: {}]", error.captured, error.reason),
    }
    eprintln!("--- quarto-error {stream} end test={name} ---");
}

fn failure_details(outcome: &FixtureOutcome<()>) -> String {
    format!(
        "work={:?}; wait={:?}; termination={:?}; entry={:?}; stdout={:?}; stderr={:?}",
        outcome.work, outcome.wait, outcome.termination, outcome.entry,
        outcome.streams.stdout, outcome.streams.stderr,
    )
}

fn require_observations(outcome: &FixtureOutcome<()>) {
    assert!(
        outcome.work.is_ok() && outcome.termination.is_ok() && outcome.entry.is_ok()
            && outcome.output.is_ok() && outcome.streams.stdout.is_ok() && outcome.streams.stderr.is_ok(),
        "incomplete Quarto error fixture observations: {}", failure_details(outcome),
    );
}

#[test]
fn fixture_runs_while_parent_environment_lock_is_held() {
    const NAME: &str = "pages::ops::error_tests::fixture_runs_while_parent_environment_lock_is_held";
    if std::env::var(LOCK_ACTOR).as_deref() == Ok(NAME) {
        test_isolated::enter(NAME);
        let _held = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{LOCK_READY}").unwrap();
        stdout.flush().unwrap();
        drop(stdout);
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "{LOCK_STDERR}").unwrap();
        stderr.flush().unwrap();
        drop(stderr);
        retains_each_error_line_and_the_last_ten_lines();
        return;
    }
    let outcome = supervise(NAME, LOCK_ACTOR, Duration::from_secs(45));
    require_observations(&outcome);
    let (stdout, stderr) = outcome.output.as_ref().unwrap();
    assert!(stdout.contains(LOCK_READY) && stderr.contains(LOCK_STDERR),
        "missing held-lock prerequisites: {}", failure_details(&outcome));
    match &outcome.wait {
        Err(error) if matches!(error.kind, ChildWaitKind::Expired) => {
            println!("quarto-error-fixture probe: lock=held stdout=present stderr=present entry=once cleanup=confirmed wait=expired");
            panic!("the diagnostic fixture blocked on ENV_TEST_LOCK before its handler timeout; {}", failure_details(&outcome));
        }
        Ok(status) if status.success() => {
            println!("quarto-error-fixture probe: lock=held stdout=present stderr=present entry=once cleanup=confirmed wait=exited status=success");
        }
        _ => panic!("wrong-cause held-lock result: {}", failure_details(&outcome)),
    }
}

fn install_stand_ins(dir: &std::path::Path, stderr: &[u8]) {
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    sot_log::test_exec::write_executable(&bin.join("julia"), "#!/bin/sh\nexit 0\n");
    let bytes = stderr.iter().map(|byte| format!("\\0{byte:03o}")).collect::<String>();
    let body = r#"#!/bin/sh
set -eu
printf 'render\n' >> invocations
out=''
while [ "$#" -gt 0 ]; do
    if [ "$1" = '--output' ]; then
        shift
        out=$1
    fi
    shift
done
test -n "$out"
printf 'partial HTML\n' > "$out"
printf '%b' 'FIXTURE_BYTES' >&2
printf 'stderr-written\n' > completed
exit 1
"#;
    sot_log::test_exec::write_executable(
        &bin.join("quarto"),
        body.replace("FIXTURE_BYTES", &bytes),
    );
    std::fs::write(dir.join("doc.qmd"), "---\nengine: julia\n---\n").unwrap();
    let mut paths = vec![bin.clone()];
    if let Some(preserved) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&preserved));
    }
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    std::env::set_var("SOT_JULIA_BIN", bin.join("julia"));
    std::env::remove_var("QUARTO_JULIA");
}

fn failed_render(stderr: &[u8], execute: bool) -> String {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = [EnvGuard::capture("PATH"), EnvGuard::capture("SOT_JULIA_BIN"), EnvGuard::capture("QUARTO_JULIA")];
    let dir = tempfile::tempdir().unwrap();
    assert!(dir.path().is_absolute());
    // SAFETY: querying this thread's signal mask changes no disposition or mask.
    let blocked = unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut mask), 0);
        libc::sigismember(&mask, libc::SIGCHLD) == 1
    };
    assert!(!blocked, "the handler fixture inherited blocked SIGCHLD");
    install_stand_ins(dir.path(), stderr);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let session = Session::new();
    let replies = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(5),
            handle_quarto_open(
                REQUEST_ID,
                serde_json::json!({ "path": dir.path().join("doc.qmd"), "execute": execute }),
                &session,
            ),
        ).await
    }).unwrap_or_else(|error| panic!(
        "the handler exceeded its five-second bound: {error}; stub entered={}, stderr written={}",
        dir.path().join("invocations").exists(), dir.path().join("completed").exists(),
    )).expect("handler reply");
    assert_eq!(replies.len(), 1, "one failure reply");
    let (frame, blob) = &replies[0];
    assert_eq!(frame.id, REQUEST_ID);
    assert_eq!(frame.op, sot_protocol::op::QUARTO_OPEN);
    assert_eq!(frame.kind, sot_protocol::Kind::Res);
    assert_eq!(frame.payload["code"], "quarto_render_failed");
    assert!(blob.is_none(), "a failed render carries no blob");
    assert_eq!(std::fs::read_to_string(dir.path().join("invocations")).unwrap(), "render\n", "render once");
    assert!(!dir.path().join(format!("__sot-qmd-{REQUEST_ID}.html")).exists(), "partial HTML removed");
    frame.payload["error"].as_str().expect("error text").to_owned()
}

#[test]
fn retains_each_error_line_and_the_last_ten_lines() {
    if !isolated("pages::ops::error_tests::retains_each_error_line_and_the_last_ten_lines") {
        return;
    }
    let repeated = "01 ERROR repeated cause";
    let colored = "\u{1b}[31m03 ERROR colored cause\u{1b}[0m";
    let mut lines = vec![repeated.to_owned(), "02 ordinary outside tail".to_owned(), colored.to_owned()];
    lines.push(repeated.to_owned());
    lines.extend((5..=13).map(|n| format!("{n:02} ordinary outside tail")));
    let tail: Vec<_> = (14..=23).map(|n| {
        if n == 17 { "17 ERROR tail cause".to_owned() } else { format!("{n:02} stack frame") }
    }).collect();
    lines.extend(tail.iter().cloned());
    let stderr = format!("{}\n", lines.join("\n"));
    let message = failed_render(stderr.as_bytes(), true);
    assert!(message.contains(repeated), "retained ERROR text is missing: {message}");
    let expected = format!("{repeated}\n{colored}\n{repeated}\n{}", tail.join("\n"));
    assert_eq!(message.strip_prefix("quarto render failed (execute=true):\n"), Some(expected.as_str()), "the entire stderr selection");
    assert_eq!(message.matches("17 ERROR tail cause").count(), 1, "tail overlap included once");
    assert_eq!(message.matches(repeated).count(), 2, "identical ERROR occurrences both retained");
}

#[test]
fn names_the_transport_race_and_upgrade() {
    if !isolated("pages::ops::error_tests::names_the_transport_race_and_upgrade") {
        return;
    }
    let error = format!("ERROR: {TRANSPORT_ERROR}");
    let tail = (1..=10).map(|n| format!("stack frame {n}")).collect::<Vec<_>>().join("\n");
    let stderr = format!("{error}\n{tail}\n");
    for execute in [true, false] {
        let message = failed_render(stderr.as_bytes(), execute);
        assert!(message.contains(UPGRADE), "upgrade explanation is missing: {message}");
        assert_eq!(message, format!("quarto render failed (execute={execute}):\n{error}\n{tail}\n{UPGRADE}"));
    }
}

#[test]
fn other_failures_do_not_name_the_transport_race() {
    if !isolated("pages::ops::error_tests::other_failures_do_not_name_the_transport_race") {
        return;
    }
    for stderr in ["ERROR unrelated failure\nstack frame\n", "ordinary failure\n"] {
        let message = failed_render(stderr.as_bytes(), true);
        assert_eq!(message, format!("quarto render failed (execute=true):\n{}", stderr.trim_end_matches('\n')));
        assert!(!message.contains(UPGRADE));
        assert!(!message.contains("1.9.38"));
    }
}

#[test]
fn stderr_boundaries_keep_the_existing_failure_prefix() {
    if !isolated("pages::ops::error_tests::stderr_boundaries_keep_the_existing_failure_prefix") {
        return;
    }
    let cases: &[(&[u8], &str)] = &[
        (b"", ""),
        (b" \t\n\r\n", ""),
        (b"short stderr", ":\nshort stderr"),
        (b"first\r\nsecond\r\n", ":\nfirst\nsecond"),
        (b"trailing newline\n", ":\ntrailing newline"),
        (b"ERROR invalid \xff\nframe\n", ":\nERROR invalid \u{fffd}\nframe"),
    ];
    for (stderr, suffix) in cases {
        let message = failed_render(stderr, false);
        assert_eq!(message, format!("quarto render failed (execute=false){suffix}"));
    }
}
