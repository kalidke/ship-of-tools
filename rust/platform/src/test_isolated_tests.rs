use super::*;
use crate::test_wait::FAILURE_STREAM_BYTES;

/// Which job `a_role` does in a child; unset in an ordinary run.
const ROLE: &str = "SOT_TEST_ISOLATED_ROLE";

/// A wrapped fixture's record is where a root child can create it: not in the world-writable sticky system temp
/// folder, whose `fs.protected_regular` refuses `O_CREAT` of a file another account owns. The record's folder is
/// owned by the caller, private, not sticky, and goes away with the entry.
#[cfg(unix)]
#[test]
fn a_wrapped_record_lives_in_a_private_non_sticky_folder() {
    use std::os::unix::fs::MetadataExt;
    let (_, mut entry) = test_command(
        "test_isolated::tests::a_wrapped_record_lives_in_a_private_non_sticky_folder",
    );
    entry.prepare_wrapped_record().unwrap();
    let (name, record) = entry.environment_assignment();
    assert_eq!(name, ENTERED);
    let record = PathBuf::from(record);
    let folder = record.parent().unwrap().to_path_buf();
    assert_ne!(
        folder,
        std::env::temp_dir(),
        "the record sits directly in the shared temp folder"
    );
    let meta = std::fs::metadata(&folder).unwrap();
    assert_eq!(
        meta.mode() & 0o7777,
        0o700,
        "the record's folder is sticky or open to others"
    );
    // SAFETY: geteuid has no preconditions.
    assert_eq!(meta.uid(), unsafe { libc::geteuid() });
    assert_eq!(std::fs::metadata(&record).unwrap().len(), 0);
    // What `enter` does in the child, in the child's own account: append to the existing record.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&record)
        .unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.assert_once(0)))
            .is_err()
    );
    assert!(!folder.exists(), "the record's folder outlived its entry");
}

/// The control: a real body, selected by its exact name, enters exactly once, and the test passes.
#[test]
fn a_real_body_enters_exactly_once() {
    if !run_isolated("test_isolated::tests::a_real_body_enters_exactly_once") {
        return;
    }
}

/// A name that is not the exact libtest name runs no test, and its child exits 0: that fails, never passes.
#[test]
fn an_unqualified_name_fails_as_a_body_that_did_not_enter() {
    assert_fails_with("isolated body did not enter", || {
        run_isolated("a_real_body_enters_exactly_once");
    });
}

/// The same for a misspelled qualified name.
#[test]
fn a_misspelled_name_fails_as_a_body_that_did_not_enter() {
    assert_fails_with("isolated body did not enter", || {
        run_isolated("test_isolated::tests::a_real_body_enters_exactly_onse");
    });
}

/// The same for a role a parent starts with `test_command`: a child that exited 0 having run no test fails.
#[test]
fn a_role_that_selects_no_test_fails_as_a_body_that_did_not_enter() {
    let (mut command, entry) = test_command("test_isolated::tests::no_such_role");
    let mut child = command.stdin(Stdio::null()).spawn().expect("spawn");
    assert!(
        wait_within(&mut child, ISOLATION_TIMEOUT).success(),
        "a zero-test child exits 0"
    );
    let pid = child.id();
    assert_fails_with("isolated body did not enter", || entry.assert_once(pid));
}

/// The check is not optional: an entry record dropped unchecked fails.
#[test]
fn an_entry_dropped_unchecked_fails() {
    let (_command, entry) =
        test_command("test_isolated::tests::a_real_body_enters_exactly_once");
    assert_fails_with("dropped without assert_once", || drop(entry));
}

/// The child the drain and bound tests re-run; in an ordinary run, nothing.
#[test]
fn a_role() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    if role != "zero-release" {
        enter("test_isolated::tests::a_role");
    }
    if let Some(path) = std::env::var_os("SOT_TEST_FIXTURE_STARTED") {
        let mut record =
            fixture_start_record("test_isolated::tests::a_role", std::process::id());
        match std::env::var("SOT_TEST_FIXTURE_WITNESS").as_deref() {
            Ok("pid") => record = fixture_start_record("test_isolated::tests::a_role", 0),
            Ok("role") => {
                record =
                    fixture_start_record("test_isolated::tests::wrong_role", std::process::id())
            }
            Ok("partial") => {
                record = "fixture-start test=test_isolated::tests::a_role\n".into()
            }
            _ => {}
        }
        std::fs::write(path, record + "fixture-start-written\n").unwrap();
    }
    match role.as_str() {
        // More than any platform's pipe holds (64 KiB on Linux and macOS, about 4 KiB on Windows), then exit.
        "flood" => {
            let mut out = std::io::stdout().lock();
            for _ in 0..64 {
                out.write_all(&[b'x'; 4096]).expect("write the flood");
            }
        }
        "stall" => std::thread::sleep(Duration::from_secs(120)),
        // Says something on both streams, more than a failure carries on stdout, then never ends.
        "speak-then-stall" => {
            eprintln!("said on stderr before the stall");
            std::io::stderr().flush().unwrap();
            let mut out = std::io::stdout().lock();
            for _ in 0..16 {
                out.write_all(&[b'x'; 4096]).expect("write the flood");
            }
            writeln!(out, "\nsaid on stdout before the stall").unwrap();
            out.flush().unwrap();
            drop(out);
            std::thread::sleep(Duration::from_secs(120));
        }
        "bytes" => {
            let mut err = std::io::stderr().lock();
            err.write_all(b"intended child diagnostic \xFF\xFE\xC3(\n")
                .unwrap();
            err.flush().unwrap();
            let mut out = std::io::stdout().lock();
            writeln!(out, "valid stdout control\nutf8-start test=test_isolated::tests::a_role child={} intended child failure", std::process::id()).unwrap();
            out.flush().unwrap();
            panic!("intended child failure");
        }
        "byte-controls" => {
            let mut out = std::io::stdout().lock();
            for bytes in [b"\xE2".as_slice(), b"\x82\xAC\n", b"\x80\xC3(\xE2\x82"] {
                out.write_all(bytes).unwrap();
                out.flush().unwrap();
            }
        }
        "release" | "zero-release" => {
            eprintln!("fixture-start observed");
            std::io::stderr().flush().unwrap();
            std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
        }
        // Leaves a descendant that holds this child's stdin and stdout, and exits at once.
        "leave" => {
            Command::new(std::env::current_exe().expect("current_exe"))
                .args([
                    "--exact",
                    "test_isolated::tests::a_role",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(ROLE, "hold")
                .env_remove(ENTERED)
                .spawn()
                .expect("spawn the descendant");
        }
        // The descendant: holds its inherited stdout until its inherited stdin ends.
        "hold" => {
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
        }
        other => panic!("unknown role {other}"),
    }
}

/// A child that writes more than its pipe holds is drained while it runs, so it finishes and is read whole.
#[test]
fn a_child_that_fills_its_pipe_is_drained_and_finishes() {
    let (mut command, entry) = test_command("test_isolated::tests::a_role");
    let child = command
        .env(ROLE, "flood")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let pid = child.id();
    let (status, stdout, _) = drain(child).wait_within(ISOLATION_TIMEOUT);
    assert!(status.success(), "{status}");
    assert!(
        stdout.len() >= 64 * 4096,
        "the flood was not read whole: {} bytes",
        stdout.len()
    );
    entry.assert_once(pid);
}

/// A drained child that hangs fails its parent with what it had said, through the panicking and the returning wait
/// alike: the deadline, confirmed cleanup, then both streams read so far, each cut to its last
/// `FAILURE_STREAM_BYTES` so a flood cannot fill the log. Each form waits on a fresh child of its own.
#[test]
fn a_hanging_drained_child_fails_with_its_streams_in_both_forms() {
    let forms: [(&str, fn(Draining) -> String); 2] = [
        ("panicking", |draining| {
            let wait = || draining.wait_within(Duration::from_secs(5));
            let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(wait));
            panic_text(payload.expect_err("a child that never ends must fail its wait"))
        }),
        ("returning", |draining| {
            let result = draining.try_wait_within(Duration::from_secs(5));
            result.expect_err("a child that never ends must return a failed wait")
        }),
    ];
    for (form, wait) in forms {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command
            .env(ROLE, "speak-then-stall")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let text = wait(drain(child));
        assert!(text.contains("did not complete within its absolute work deadline"), "{form}: {text}");
        assert!(text.contains("cleanup=confirmed"), "{form}: {text}");
        assert!(text.contains("said on stderr before the stall"), "{form}: the stderr read so far is missing: {text}");
        assert!(text.contains("said on stdout before the stall"), "{form}: the stdout read so far is missing: {text}");
        assert!(text.contains(&format!("last {FAILURE_STREAM_BYTES} of ")), "{form}: the stdout flood was not cut: {text}");
        assert!(text.len() < 3 * FAILURE_STREAM_BYTES, "{form}: the failure carries {} bytes", text.len());
        entry.assert_once(pid);
        println!("drained-hang-proof form={form} streams=retained cleanup=confirmed entry=once");
    }
}

/// A child that never ends fails its parent at the bound, by name, and is killed.
#[test]
fn a_stalled_child_fails_at_its_bound() {
    let (mut command, entry) = test_command("test_isolated::tests::a_role");
    let child = command
        .env(ROLE, "stall")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut started = None;
    let outcome = supervise_fixture_until(
        child,
        entry,
        Instant::now() + ISOLATION_TIMEOUT,
        |child, entry, deadline| {
            observe_entry(entry, child.id(), *deadline)?;
            let origin = Instant::now();
            started = Some(origin);
            *deadline = origin + Duration::from_secs(5);
            Ok(())
        },
    );
    outcome.report("test_isolated::tests::a_role");
    assert!(outcome.work.is_ok() && outcome.entry.is_ok() && outcome.termination.is_ok());
    assert!(matches!(
        outcome.wait.unwrap_err().kind,
        ChildWaitKind::Expired
    ));
    assert!(
        started.unwrap().elapsed() < Duration::from_secs(15),
        "the bound was not kept"
    );
}

/// A held output stream fails after the child has exited; entry/end checks still finish.
#[test]
fn output_a_descendant_holds_fails_at_the_bound() {
    let (mut command, entry) = test_command("test_isolated::tests::a_role");
    let child = command
        .env(ROLE, "leave")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut input = None;
    let mut started = None;
    let outcome = supervise_fixture_until(
        child,
        entry,
        Instant::now() + ISOLATION_TIMEOUT,
        |child, entry, deadline| {
            observe_entry(entry, child.id(), *deadline)?;
            input = child.stdin.take();
            let origin = Instant::now();
            started = Some(origin);
            *deadline = origin + Duration::from_secs(5);
            Ok(())
        },
    );
    drop(input);
    outcome.report("test_isolated::tests::a_role");
    assert!(outcome.work.is_ok() && outcome.entry.is_ok() && outcome.termination.is_ok());
    assert!(outcome.wait.unwrap().success());
    assert!(outcome.output.unwrap_err().contains("did not end within"));
    assert!(
        started.unwrap().elapsed() < Duration::from_secs(15),
        "the bound was not kept"
    );
}

fn assert_fails_with(text: &str, check: impl FnOnce()) {
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check))
        .expect_err("a check that must fail passed");
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or("");
    assert!(message.contains(text), "{message}");
}

fn observe_entry(entry: &Entry, pid: u32, deadline: Instant) -> Result<(), FixtureFailure> {
    loop {
        let record = match std::fs::read_to_string(&entry.path) {
            Ok(record) => record,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(FixtureFailure::Error(error.to_string())),
        };
        if record == format!("{} {pid}\n", entry.name) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(FixtureFailure::Timeout(
                "complete entry readiness record missing".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn observe_record(
    path: &std::path::Path,
    deadline: Instant,
    marker: &str,
) -> Result<String, FixtureFailure> {
    loop {
        let text = std::fs::read_to_string(path).map_err(|e| {
            eprintln!("fixture-io kind={:?} reason={}", e.kind(), e);
            FixtureFailure::Error(format!("readiness read failed: {:?}", e.kind()))
        })?;
        if text
            .split_inclusive('\n')
            .any(|line| line.ends_with('\n') && line.starts_with(marker))
        {
            return Ok(text);
        }
        if Instant::now() >= deadline {
            return Err(FixtureFailure::Timeout(
                "expected readiness marker missing".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn readiness_fixture(
    selected: ReadinessCase,
    control: &str,
    role: &str,
) -> (FixtureOutcome<()>, bool) {
    let started = tempfile::NamedTempFile::new().unwrap();
    let (mut command, entry) = test_command("test_isolated::tests::a_role");
    let child = command
        .env(ROLE, role)
        .env("SOT_TEST_FIXTURE_STARTED", started.path())
        .env("SOT_TEST_FIXTURE_WITNESS", control)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut start = false;
    let outcome = supervise_fixture_until(
        child,
        entry,
        Instant::now() + ISOLATION_TIMEOUT,
        |_, _, end| {
            let path = if control == "before" {
                started.path().with_extension("missing")
            } else {
                started.path().to_path_buf()
            };
            let witness = observe_record(&path, *end, "fixture-start-written")?;
            start = fixture_start_matches(&witness, "test_isolated::tests::a_role", pid);
            eprintln!("fixture-witness child={pid} record={witness:?}");
            let readiness_end = Instant::now() + Duration::from_millis(150);
            *end = readiness_end + Duration::from_millis(600);
            readiness_failure(
                selected,
                control,
                &started.path().with_extension("missing"),
                || observe_record(started.path(), readiness_end, "withheld-ready").map(|_| ()),
            )
        },
    );
    (outcome, start)
}
#[test]
fn readiness_failure_finishes_owned_child_checks() {
    for (case, held, zero) in readiness_scenarios() {
        let role = if held {
            "stall"
        } else if zero {
            "zero-release"
        } else {
            "release"
        };
        let (outcome, start) = readiness_fixture(case, "valid", role);
        verify_readiness(
            &outcome,
            "test_isolated::tests::a_role",
            start,
            case,
            role == "stall",
            role == "zero-release",
        );
    }
}
#[test]
fn readiness_proof_rejects_wrong_failure() {
    verify_readiness_controls("test_isolated::tests::a_role", |case, control| {
        readiness_fixture(case, control, "release")
    });
}

#[test]
fn byte_output_controls_preserve_valid_spans_and_invalid_stdout() {
    let (mut command, entry) = test_command("test_isolated::tests::a_role");
    let child = command
        .env(ROLE, "byte-controls")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let outcome = supervise_fixture_until(
        child,
        entry,
        Instant::now() + ISOLATION_TIMEOUT,
        |_, _, _| Ok(()),
    );
    outcome.report("test_isolated::tests::a_role");
    assert!(outcome.termination.is_ok() && outcome.entry.is_ok());
    assert!(outcome.wait.unwrap().success());
    let (out, _) = outcome.output.unwrap();
    assert!(out.contains("€\n\\x80\\xC3(\\xE2\\x82"), "{out}");
}

fn byte_fixture() -> FixtureOutcome<()> {
    let (mut command, entry) = test_command("test_isolated::tests::a_role");
    let child = command
        .env(ROLE, "bytes")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    supervise_fixture_until(
        child,
        entry,
        Instant::now() + ISOLATION_TIMEOUT,
        |_, _, _| Ok(()),
    )
}
// Independent child prerequisites precede decoder classification, including in every negative control.
fn verify_utf8(
    outcome: &FixtureOutcome<()>,
    injected: Option<&OutputFailure>,
) -> Result<(), &'static str> {
    let role = "test_isolated::tests::a_role";
    outcome.report(role);
    let out = outcome
        .streams
        .stdout
        .as_ref()
        .map_err(|_| "UTF-8 fixture stdout failed")?;
    let witness = format!(
        "utf8-start test={role} child={} intended child failure\n",
        outcome.child
    );
    assert!(
        outcome.wait.as_ref().is_ok_and(|s| s.code() == Some(101))
            && outcome.termination.is_ok()
            && outcome.entry.is_ok()
            && out.contains("valid stdout control\n")
            && out.split_inclusive('\n').any(|line| line == witness),
        "UTF-8 fixture prerequisites failed"
    );
    eprintln!(
        "utf8-child child={} status={} cleanup=confirmed entry=once witness={witness:?}",
        outcome.child,
        outcome.wait.as_ref().unwrap()
    );
    let failure = injected.or_else(|| outcome.streams.stderr.as_ref().err());
    if let Some(failure) = failure {
        let decoder = failure.stream == "stderr"
            && failure.kind == Some(std::io::ErrorKind::InvalidData)
            && failure.reason == "stream did not contain valid UTF-8";
        eprintln!("utf8-cause test={role} child={} status={} cleanup=confirmed entry=once stream={} kind={:?} reason={} decoder={}",
            outcome.child, outcome.wait.as_ref().unwrap(), failure.stream, failure.kind, failure.reason, if decoder { "matched" } else { "rejected" });
        return Err(if decoder {
            "child output lost escaped invalid UTF-8 diagnostic"
        } else {
            "UTF-8 proof observed an unrelated capture failure"
        });
    }
    let err = outcome.streams.stderr.as_ref().unwrap();
    assert!(
        err.contains("intended child diagnostic \\xFF\\xFE\\xC3(")
            && err.contains("intended child failure"),
        "child output lost escaped invalid UTF-8 diagnostic: {err}"
    );
    eprintln!(
        "{err}child-status={} child={} bodies=1 cleanup=confirmed",
        outcome.wait.as_ref().unwrap(),
        outcome.child
    );
    Ok(())
}
#[test]
fn invalid_utf8_stderr_preserves_the_child_diagnostic() {
    verify_utf8(&byte_fixture(), None).unwrap_or_else(|reason| panic!("{reason}"));
}
#[test]
fn utf8_proof_rejects_wrong_capture_failure() {
    for (stream, kind, reason) in [
        (
            "stderr",
            std::io::ErrorKind::Other,
            "unrelated output observation failure",
        ),
        (
            "stdout",
            std::io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        ),
        (
            "stderr",
            std::io::ErrorKind::InvalidData,
            "another decoder reason",
        ),
    ] {
        let outcome = byte_fixture();
        let failure = OutputFailure {
            stream,
            kind: Some(kind),
            reason: reason.into(),
            captured: String::new(),
        };
        let rejected = verify_utf8(&outcome, Some(&failure));
        assert_eq!(
            rejected,
            Err("UTF-8 proof observed an unrelated capture failure"),
            "UTF-8 proof accepted an unrelated capture failure"
        );
        eprintln!("utf8-rejection stream={stream} kind={kind:?} reason={reason}");
    }
}

// Body entry and child cleanup are behavioral proofs; the rerun spelling catalog is retired.

#[test]
fn wait_until_retains_a_spent_deadline_and_confirms_expiry() {
    let (mut command, entry) = test_command("test_isolated::tests::a_role");
    let child = command
        .env(ROLE, "stall")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut started = None;
    let outcome = supervise_fixture_until(
        child,
        entry,
        Instant::now() + ISOLATION_TIMEOUT,
        |child, entry, deadline| {
            observe_entry(entry, child.id(), *deadline)?;
            let origin = Instant::now();
            started = Some(origin);
            *deadline = origin + Duration::from_millis(600);
            std::thread::sleep(Duration::from_millis(400));
            Ok(())
        },
    );
    outcome.report("test_isolated::tests::a_role");
    assert!(outcome.work.is_ok() && outcome.entry.is_ok() && outcome.termination.is_ok());
    let error = outcome.wait.unwrap_err();
    assert!(matches!(error.kind, ChildWaitKind::Expired) && error.termination_confirmed);
    eprintln!("body-proof test=test_isolated::tests::a_role child={} bodies=1 completed=false cleanup=confirmed", outcome.child);
    assert!(
        started.unwrap().elapsed() < Duration::from_millis(950),
        "child deadline was recomputed"
    );
}

#[test]
fn run_isolated_until_reports_successful_parent_false() {
    let child_body = run_isolated_until(
        "test_isolated::tests::run_isolated_until_reports_successful_parent_false",
        Instant::now() + ISOLATION_TIMEOUT,
    )
    .unwrap();
    if child_body {
        return;
    }
    assert!(!child_body, "successful ISO parent must be false");
}
