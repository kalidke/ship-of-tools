//! Native-account fixtures share one explicit command recipe, ISO finalization and failure verifier.

use super::*;
use sot_log::test_isolated::{
    enter, supervise_wrapped_fixture_until, FixtureOutcome, WrappedSpawnFailure,
};
use std::ffi::OsString;
use std::process::Command;

const ROLE: &str = "privileged::native_role";
const ENTRY: &str = "SOT_TEST_ISOLATED_ENTERED";
const CONTROL: &str = "SOT_TEST_NATIVE_CONTROL";

#[derive(Clone, Copy)]
enum Launcher {
    Direct,
    Wrapped,
    Startup,
    Elevated,
}

fn assignment(name: &str, value: impl AsRef<std::ffi::OsStr>) -> OsString {
    let mut assignment = OsString::from(name);
    assignment.push("=");
    assignment.push(value);
    assignment
}

/// Transfer only the identified ISO assignment and this fixture's explicit native assignments.
fn recipe(
    role: &str,
    launcher: Launcher,
    assignments: &[(&str, OsString)],
) -> Result<(Command, sot_log::test_isolated::Entry), WrappedSpawnFailure> {
    let (mut direct, entry) = test_command(role);
    if let Err(error) = entry.prepare_wrapped_record() {
        let entry = std::panic::catch_unwind(|| entry.assert_once(0)).map_err(panic_message);
        return Err(WrappedSpawnFailure { error, entry });
    }
    let (entry_name, entry_path) = entry.environment_assignment();
    let mut command = match launcher {
        Launcher::Direct => {
            for (name, value) in assignments {
                direct.env(name, value);
            }
            direct
        }
        Launcher::Startup => {
            let mut command = Command::new("/bin/sh");
            command.args([
                "-c",
                "printf 'startup-stdout-marker\\n'; printf 'startup-stderr-marker\\n' >&2; exit 73",
            ]);
            command.env(entry_name, entry_path);
            command
        }
        Launcher::Wrapped => {
            let mut command = Command::new("/bin/sh");
            command.args([
                "-c",
                "\"$@\" & native=$!; wait \"$native\"",
                "native-wrapper",
            ]);
            command.arg(direct.get_program()).args(direct.get_args());
            command.env(entry_name, entry_path);
            for (name, value) in assignments {
                command.env(name, value);
            }
            command
        }
        Launcher::Elevated => {
            let mut command = Command::new("sudo");
            command.args(["-n", "--", "env"]);
            command.arg(assignment(entry_name, entry_path));
            for (name, value) in assignments {
                command.arg(assignment(name, value));
            }
            command.arg(direct.get_program()).args(direct.get_args());
            command
        }
    };
    command.stdin(Stdio::null());
    Ok((command, entry))
}

type NativeOutcome = Result<FixtureOutcome<()>, WrappedSpawnFailure>;

fn run_fixture(
    role: &str,
    launcher: Launcher,
    assignments: &[(&str, OsString)],
    wait: &WaitContext,
) -> NativeOutcome {
    let observed = match recipe(role, launcher, assignments) {
        Ok((command, entry)) => {
            supervise_wrapped_fixture_until(command, entry, wait.deadline, role)
        }
        Err(error) => Err(error),
    };
    match &observed {
        Ok(outcome) => wait.child_outcome(&outcome.wait),
        Err(error) => wait.complete("error", Some(&error.error), None),
    }
    observed
}

fn stream_text(stream: &Result<String, sot_log::test_isolated::OutputFailure>) -> String {
    match stream {
        Ok(text) if text.is_empty() => "<empty>".into(),
        Ok(text) => text.clone(),
        Err(error) => format!("capture-error: {error}"),
    }
}

fn spawn_failure_report(role: &str, failure: &WrappedSpawnFailure, wait: &WaitContext) -> String {
    wait.emit(&format!("native-fixture test={role} launcher=none native=missing status=not-spawned wait=error cleanup=unconfirmed entry=failed"));
    wait.emit(&format!(
        "native-error phase=spawn kind={:?} errno={} reason={}",
        failure.error.kind(),
        failure
            .error
            .raw_os_error()
            .map_or("none".into(), |n| n.to_string()),
        failure.error
    ));
    wait.emit(&format!(
        "native-error phase=entry kind=InvalidData errno=none reason={:?}",
        failure.entry
    ));
    wait.emit("native-stdout <empty>");
    wait.emit("native-stderr <empty>");
    let report = format!("native fixture did not enter; launcher status=not-spawned; stdout=<empty>; stderr=<empty>; entry={:?}; spawn={}", failure.entry, failure.error);
    wait.emit(&report);
    report
}

/// Construct the emitted report only after the owner has attempted all mandatory observations.
fn failure_report(role: &str, observed: &NativeOutcome, wait: &WaitContext) -> Option<String> {
    let outcome = match observed {
        Err(failure) => return Some(spawn_failure_report(role, failure, wait)),
        Ok(outcome) => outcome,
    };
    let native = outcome
        .native
        .as_ref()
        .and_then(|pid| pid.as_ref().ok())
        .copied();
    let status = outcome
        .wait
        .as_ref()
        .map_or_else(|e| format!("{e}"), ToString::to_string);
    let stdout = stream_text(&outcome.streams.stdout);
    let stderr = stream_text(&outcome.streams.stderr);
    wait.emit(&format!("native-fixture test={role} launcher={} native={} status={status} wait={} cleanup={} entry={}",
        outcome.child, native.map_or("missing".into(), |n| n.to_string()),
        match &outcome.wait { Ok(_) => "exited", Err(e) if matches!(e.kind, ChildWaitKind::Expired) => "expired", Err(_) => "error" },
        if outcome.termination.is_ok() { "confirmed" } else { "unconfirmed" }, if outcome.entry.is_ok() { "once" } else { "failed" }));
    if let Some(Err(error)) = &outcome.native {
        wait.emit(&format!(
            "native-error phase=start kind=InvalidData errno=none reason={error}"
        ));
    }
    let report = if let Err(error) = &outcome.entry {
        wait.emit(&format!(
            "native-error phase=entry kind=InvalidData errno=none reason={error}"
        ));
        Some(format!("native fixture did not enter; launcher status={status}; stdout={stdout}; stderr={stderr}; entry={error}"))
    } else if !outcome
        .wait
        .as_ref()
        .is_ok_and(std::process::ExitStatus::success)
        || outcome.termination.is_err()
        || outcome.output.is_err()
        || outcome.work.is_err()
    {
        Some(format!("native fixture failed; launcher status={status}; stdout={stdout}; stderr={stderr}; entry=once; termination={:?}; output={:?}; work={:?}", outcome.termination, outcome.output, outcome.work))
    } else {
        None
    };
    if let Err(error) = &outcome.wait {
        let (kind, errno) = match &error.kind {
            ChildWaitKind::Expired => (std::io::ErrorKind::TimedOut, None),
            ChildWaitKind::Poll(error) => (error.kind(), error.raw_os_error()),
        };
        wait.emit(&format!("native-error phase=wait kind={kind:?} errno={} reason={error}; native termination unconfirmed; launcher cleanup alone proves no native completion",
            errno.map_or("none".into(), |n| n.to_string())));
    }
    emit_output_failures(outcome, wait);
    wait.emit(&format!("native-stdout {stdout}"));
    wait.emit(&format!("native-stderr {stderr}"));
    if let Some(report) = &report {
        wait.emit(report);
    }
    report
}

fn emit_output_failures(outcome: &FixtureOutcome<()>, wait: &WaitContext) {
    for stream in [&outcome.streams.stdout, &outcome.streams.stderr] {
        if let Err(error) = stream {
            wait.emit(&format!(
                "native-error phase=output kind={} errno=none reason={error}",
                error.kind.map_or("none".into(), |kind| format!("{kind:?}"))
            ));
        }
    }
}

fn native_start(role: &str, control: &str) {
    let pid = std::process::id();
    let record = match control {
        "wrong-role" => format!("native-start test=privileged::wrong_role native={pid}\n"),
        "wrong-pid" => format!("native-start test={role} native={}\n", pid + 1),
        "partial-start" => format!("native-start test={role} native="),
        _ => format!("native-start test={role} native={pid}\n"),
    };
    let mut out = std::io::stdout().lock();
    out.write_all(record.as_bytes()).unwrap();
    if control == "duplicate-start" {
        out.write_all(record.as_bytes()).unwrap();
    }
    out.flush().unwrap();
}

fn entered_control(role: &str, control: &str) {
    match control {
        "zero" => {}
        "no-entry" => {}
        "partial-entry" => {
            std::fs::write(std::env::var_os(ENTRY).unwrap(), format!("{role} ")).unwrap()
        }
        "duplicate-entry" => {
            enter(role);
            enter(role);
        }
        _ => enter(role),
    }
}

#[test]
fn native_role() {
    let Ok(control) = std::env::var(CONTROL) else {
        return;
    };
    if control != "zero" {
        native_start(ROLE, &control);
    }
    entered_control(ROLE, &control);
    if control == "prerequisite" || control == "wrong-cause" {
        let reason = if control == "prerequisite" {
            "selected native prerequisite unavailable"
        } else {
            "unrelated native prerequisite unavailable"
        };
        eprintln!("native-error phase=privilege kind=PermissionDenied errno=none reason={reason}");
        std::io::stderr().flush().unwrap();
        std::process::exit(74);
    }
    println!("native-control completion=success");
}

fn control_fixture(control: &str, launcher: Launcher) -> (NativeOutcome, WaitContext) {
    let wait = WaitContext::new(
        ROLE,
        "native.control",
        "retained status and entry",
        None,
        TIMEOUT,
    );
    let outcome = run_fixture(ROLE, launcher, &[(CONTROL, control.into())], &wait);
    (outcome, wait)
}

fn isolated(test: &str) -> bool {
    sot_log::test_isolated::run_isolated(test)
}

#[test]
fn startup_failure_reports_status_and_capture() {
    let test = "privileged::startup_failure_reports_status_and_capture";
    if !isolated(test) {
        return;
    }
    let (observed, wait) = control_fixture("startup", Launcher::Startup);
    let outcome = observed.as_ref().unwrap();
    assert_eq!(
        outcome.wait.as_ref().unwrap().code(),
        Some(73),
        "startup prerequisite status changed"
    );
    assert!(outcome.termination.is_ok() && outcome.entry.is_err());
    assert!(outcome
        .streams
        .stdout
        .as_ref()
        .unwrap()
        .contains("startup-stdout-marker"));
    assert!(outcome
        .streams
        .stderr
        .as_ref()
        .unwrap()
        .contains("startup-stderr-marker"));
    // Status/capture prerequisites already hold before inspecting the old callee's panic.
    let emitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        failure_report(ROLE, &observed, &wait)
    }));
    let report = match emitted {
        Ok(report) => report.expect("privileged fixture accepted missing native entry"),
        Err(panic) => panic_message(panic),
    };
    wait.emit(&report);
    assert!(
        report.contains("73")
            && report.contains("startup-stdout-marker")
            && report.contains("startup-stderr-marker")
            && report.contains("native fixture did not enter"),
        "privileged fixture failure lost status or captured cause"
    );
    eprintln!(
        "native-report-proof controller={test} bodies=1 status=73 native=missing cleanup=confirmed"
    );
}

#[test]
fn entry_failure_never_counts_as_native_proof() {
    let test = "privileged::entry_failure_never_counts_as_native_proof";
    if !isolated(test) {
        return;
    }
    for control in [
        "zero",
        "no-entry",
        "partial-entry",
        "duplicate-entry",
        "wrong-role",
        "wrong-pid",
        "partial-start",
        "duplicate-start",
        "valid",
    ] {
        let (observed, wait) = control_fixture(control, Launcher::Wrapped);
        let outcome = observed.as_ref().unwrap();
        assert!(
            outcome.wait.as_ref().unwrap().success()
                && outcome.termination.is_ok()
                && outcome.output.is_ok()
        );
        let report = failure_report(ROLE, &observed, &wait);
        if control == "valid" {
            assert!(report.is_none() && outcome.entry.is_ok());
            assert_ne!(
                outcome.child,
                *outcome.native.as_ref().unwrap().as_ref().unwrap()
            );
        } else {
            assert!(
                report.is_some() && outcome.entry.is_err(),
                "privileged fixture accepted missing native entry"
            );
            let report = report.unwrap();
            assert!(
                report.contains("launcher status=")
                    && report.contains("stdout=")
                    && report.contains("stderr=")
            );
        }
        eprintln!("native-entry-control control={control} controller-bodies=1 launcher-ended=true");
    }
}

fn verify_native_failure(observed: &NativeOutcome, wait: &WaitContext) {
    let outcome = observed.as_ref().unwrap();
    assert!(outcome.entry.is_ok() && outcome.termination.is_ok() && outcome.output.is_ok());
    assert_eq!(outcome.wait.as_ref().unwrap().code(), Some(74));
    let report = failure_report(ROLE, observed, wait).expect("failed native prerequisite accepted");
    assert!(report.contains("native-error phase=privilege kind=PermissionDenied errno=none reason=selected native prerequisite unavailable"),
        "native fixture observed wrong prerequisite cause");
}

#[test]
fn native_failure_retains_status_and_output() {
    let test = "privileged::native_failure_retains_status_and_output";
    if !isolated(test) {
        return;
    }
    let (observed, wait) = control_fixture("prerequisite", Launcher::Direct);
    verify_native_failure(&observed, &wait);
    let (observed, wait) = control_fixture("wrong-cause", Launcher::Direct);
    let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        verify_native_failure(&observed, &wait)
    }));
    assert!(
        rejected.is_err()
            && panic_message(rejected.unwrap_err())
                .contains("native fixture observed wrong prerequisite cause")
    );
    eprintln!(
        "native-cause-proof controller={test} bodies=1 wrong-cause=rejected cleanup=confirmed"
    );
}

fn native_operation(wait: &WaitContext, phase: &str, name: &str, operation: impl FnOnce() -> i32) {
    let rc = operation();
    if rc != 0 {
        let error = std::io::Error::last_os_error();
        wait.emit(&format!(
            "native-error phase={phase} kind={:?} errno={} reason={name}: {error}",
            error.kind(),
            error
                .raw_os_error()
                .map_or("none".into(), |n| n.to_string())
        ));
        panic!("native operation {name} failed: {error}");
    }
}

pub(super) fn foreign_client_role(test: &str) -> bool {
    let Some(path) = std::env::var_os("SOT_TEST_FOREIGN_SOCKET") else {
        return false;
    };
    native_start(test, "valid");
    enter(test);
    let wait = WaitContext::new(
        test,
        "native.body",
        "credential transition and refusal",
        None,
        TIMEOUT,
    );
    let foreign: u32 = std::env::var("SOT_TEST_FOREIGN_UID")
        .unwrap()
        .parse()
        .unwrap();
    let owner: u32 = std::env::var("SOT_TEST_FOREIGN_OWNER")
        .unwrap()
        .parse()
        .unwrap();
    if unsafe { libc::geteuid() } != 0 {
        wait.emit("native-error phase=privilege kind=PermissionDenied errno=none reason=native privilege prerequisite unavailable");
        panic!("native privilege prerequisite unavailable");
    }
    native_operation(&wait, "privilege", "setgroups", || unsafe {
        libc::setgroups(0, std::ptr::null())
    });
    native_operation(&wait, "privilege", "setgid", || unsafe {
        libc::setgid(foreign)
    });
    native_operation(&wait, "privilege", "setuid", || unsafe {
        libc::setuid(foreign)
    });
    let account = unsafe { libc::geteuid() };
    assert_eq!(account, foreign);
    assert_ne!(
        account, owner,
        "native account did not change from the owner"
    );
    let pid = std::process::id();
    println!("native-credentials test={test} native={pid} foreign=true");
    std::io::stdout().flush().unwrap();
    let error = UnixStream::connect(path).expect_err("foreign account connected to private lane");
    wait.emit(&format!(
        "native-error phase=connect kind={:?} errno={} reason={error}",
        error.kind(),
        error
            .raw_os_error()
            .map_or("none".into(), |n| n.to_string())
    ));
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "native private-socket refusal was not observed"
    );
    println!("native-refusal test={test} native={pid} kind=PermissionDenied");
    std::io::stdout().flush().unwrap();
    true
}

pub(super) fn observe_foreign_denial(
    test: &str,
    path: &std::path::Path,
    foreign: u32,
    owner: u32,
    server: &SocketServer,
) {
    let wait = WaitContext::new(
        test,
        "foreign.child.wait",
        "native refusal and child completion",
        None,
        TIMEOUT,
    );
    let observed = run_fixture(
        test,
        if unsafe { libc::geteuid() } == 0 {
            Launcher::Direct
        } else {
            Launcher::Elevated
        },
        &[
            ("SOT_TEST_FOREIGN_SOCKET", path.as_os_str().to_os_string()),
            ("SOT_TEST_FOREIGN_UID", foreign.to_string().into()),
            ("SOT_TEST_FOREIGN_OWNER", owner.to_string().into()),
        ],
        &wait,
    );
    if let Some(report) = failure_report(test, &observed, &wait) {
        panic!("{report}");
    }
    let outcome = observed.unwrap();
    let native = *outcome.native.as_ref().unwrap().as_ref().unwrap();
    let stdout = outcome.streams.stdout.as_ref().unwrap();
    for record in [
        format!("native-credentials test={test} native={native} foreign=true\n"),
        format!("native-refusal test={test} native={native} kind=PermissionDenied\n"),
    ] {
        assert_eq!(
            stdout
                .split_inclusive('\n')
                .filter(|line| *line == record)
                .count(),
            1,
            "native proof witness missing or duplicated"
        );
    }
    assert!(
        matches!(
            WaitContext::from_origin(
                test,
                "foreign.no.event",
                "no lane event",
                None,
                wait.started,
                wait.deadline
            )
            .receive(server, Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "foreign client supplied lane events or receiver disconnected"
    );
    eprintln!("body-proof test={test} child={native} bodies=1 completed=true cleanup=confirmed");
}
