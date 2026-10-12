//! Entered commands exercise the native finish owner, including the default panic hook's output.
use super::*;
use sot_log::test_isolated::{drain, enter, test_command, ISOLATION_TIMEOUT};
use std::io::Write;

const RUNNER: &str = "command_tests::native_command_runner_role";
const CHILD: &str = "command_tests::native_command_child_role";
const MODE: &str = "SOT_L3_COMMAND_MODE";
const RAW_HOST: &str = "L3_SYNTHETIC_HOST_9";
const RAW_PATH: &str = "/L3_SYNTHETIC_PATH_9";

#[test]
fn prefixed_native_command_output_is_scrubbed() {
    let path = format!("{}/stdout-marker", scratch().display());
    for (input, expected) in [
        (
            "test command_tests::native_command_child_role ... On computer: L3_SYNTHETIC_HOST_9",
            "test command_tests::native_command_child_role ... On computer: <host>",
        ),
        ("On computer: L3_SYNTHETIC_HOST_9", "On computer: <host>"),
        (
            "  On computer: L3_SYNTHETIC_HOST_9",
            "  On computer: <host>",
        ),
    ] {
        let text = scrub(&format!("{input}\nordinary neighbor\n{path}"));
        assert_eq!(
            text,
            format!("{expected}\nordinary neighbor\n<scratch>/stdout-marker"),
            "prefixed native host banner was not scrubbed"
        );
        assert!(!text.contains(RAW_HOST), "raw synthetic host retained");
    }
    println!("L3 command-proof prefixed-banner=scrubbed prefix=retained");
    for ending in ["", "\n", "\r\n"] {
        let input = format!("ordinary\r\n\n\ntest role ... On computer: {RAW_HOST}{ending}");
        let expected = format!("ordinary\r\n\n\ntest role ... On computer: <host>{ending}");
        assert_eq!(
            scrub(&input),
            expected,
            "native scrub changed line terminators"
        );
        let input = format!("{input}\nL3 fixture macos detach=ok\n{path}\r\n");
        let expected =
            format!("{expected}\nL3 fixture macos detach=ok\n<scratch>/stdout-marker\r\n");
        assert_eq!(
            scrub(&input),
            expected,
            "native scrub changed line terminators"
        );
    }
    println!("L3 command-proof terminators=retained");
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
        .unwrap_or_else(|| "panic without text".into())
}

fn runner(mode: &str) -> NativeObservation {
    let (mut command, entry) = test_command(RUNNER);
    let child = command
        .env(MODE, mode)
        .env("CARGO_TARGET_DIR", RAW_PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| {
            panic!(
                "start the owned native-command runner: {}\nrunner status=unavailable\nrunner stdout: unavailable\nrunner stderr: unavailable",
                scrub(&error.to_string())
            )
        });
    let pid = child.id();
    let (status, stdout, stderr) = drain(child)
        .try_wait_within(ISOLATION_TIMEOUT)
        .unwrap_or_else(|error| panic!("native-command runner drain failed: {}", scrub(&error)));
    let observation = NativeObservation {
        status,
        stdout,
        stderr,
    };
    if let Err(payload) =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.assert_once(pid)))
    {
        panic!(
            "runner entry failed: {}\n{}",
            scrub(&panic_text(payload.as_ref())),
            observation.diagnostic("runner")
        );
    }
    observation
}

fn assert_scrubbed(observation: &NativeObservation) {
    let text = observation.combined();
    let diagnostic = observation.diagnostic("runner");
    assert!(
        text.contains("L3 command-child entered=once"),
        "child entry missing\n{diagnostic}"
    );
    assert!(
        text.contains("L3 command-child banner=emitted paths=emitted"),
        "native command prerequisite missing\n{diagnostic}"
    );
    assert!(
        text.contains("On computer: <host>"),
        "host replacement missing\n{diagnostic}"
    );
    assert!(
        text.contains("<target>/stdout-marker"),
        "stdout path replacement missing\n{diagnostic}"
    );
    assert!(
        text.contains("<target>/stderr-marker"),
        "stderr path replacement missing\n{diagnostic}"
    );
    assert!(
        !text.contains(RAW_HOST),
        "raw synthetic host reached the panic hook\n{diagnostic}"
    );
    assert!(
        !text.contains(RAW_PATH),
        "raw synthetic path reached the panic hook\n{diagnostic}"
    );
}

fn assert_failure(observation: &NativeObservation, mode: &str) {
    let text = observation.combined();
    let diagnostic = observation.diagnostic("runner");
    assert!(
        !observation.status.success(),
        "the entered failing runner succeeded\n{diagnostic}"
    );
    assert!(
        text.contains("L3 command-child entered=once"),
        "child entry missing\n{diagnostic}"
    );
    assert!(
        text.contains("L3 command-child banner=emitted paths=emitted"),
        "native command prerequisite missing\n{diagnostic}"
    );
    if mode == "stall" {
        assert!(
            text.contains("did not complete within its absolute work deadline"),
            "wrong deadline failure\n{diagnostic}"
        );
        assert!(
            text.contains("cleanup=confirmed"),
            "owned-child cleanup unconfirmed\n{diagnostic}"
        );
    } else {
        assert!(
            text.contains("L3 command-finish exit=7"),
            "wrong native exit failure\n{diagnostic}"
        );
    }
    assert_scrubbed(observation);
}

fn assert_success(observation: &NativeObservation) {
    let diagnostic = observation.diagnostic("runner");
    assert!(
        observation.status.success(),
        "the entered successful runner failed\n{diagnostic}"
    );
    assert!(
        observation.combined().contains("L3 command-finish exit=0"),
        "success exit missing\n{diagnostic}"
    );
    assert_scrubbed(observation);
}

#[test]
fn a_native_command_failure_is_scrubbed_before_it_is_reported() {
    let observation = runner("stall");
    assert_failure(&observation, "stall");
    println!("L3 command-proof deadline=observed cleanup=confirmed hook=scrubbed entry=once");
}

/// One nonzero runner shows three things: its failure is scrubbed before either panic hook prints it; the success
/// verifier's rejection of it keeps the runner's status and both streams; and the runner's own failure keeps the
/// command's status and its two streams apart.
#[test]
fn a_nonzero_native_command_failure_is_scrubbed() {
    let observation = runner("nonzero");
    assert_failure(&observation, "nonzero");
    let diagnostic = observation.diagnostic("runner");
    assert!(
        observation.status.code() == Some(101)
            && observation
                .combined()
                .contains("L3 HARNESS FAILURE: native fixture command failed"),
        "nonzero runner prerequisite missing\n{diagnostic}"
    );
    println!("L3 command-proof nonzero=7 hook=scrubbed entry=once");
    let rejected = std::panic::catch_unwind(|| assert_success(&observation));
    let payload = rejected.expect_err(&format!(
        "a failed runner passed the success verifier\n{diagnostic}"
    ));
    let failure = panic_text(payload.as_ref());
    assert!(
        failure.contains(&format!("runner status={}", observation.status))
            && failure.contains("code=Some(101) success=false")
            && failure.contains("runner stdout:")
            && failure.contains("runner stderr:")
            && failure.contains("L3 command-finish exit=7")
            && failure.contains("L3 HARNESS FAILURE: native fixture command failed")
            && !failure.contains(RAW_HOST)
            && !failure.contains(RAW_PATH),
        "success assertion discarded runner status or captured streams\n{diagnostic}"
    );
    println!("L3 command-proof failed-success=status-and-streams-retained entry=once");
    let streams = observation
        .stderr
        .split_once("command stdout:\n")
        .and_then(|(_, text)| text.split_once("\ncommand stderr:\n"));
    assert!(
        observation.stderr.contains("command status=")
            && observation.stderr.contains("code=Some(7) success=false")
            && streams.is_some_and(|(stdout, stderr)| stdout.contains("<target>/stdout-marker")
                && stderr.contains("<target>/stderr-marker")),
        "native runner discarded command status or separate streams\n{diagnostic}"
    );
    println!("L3 command-proof runner-command=status-and-streams-retained entry=once");
}

#[test]
fn successful_native_command_output_is_scrubbed() {
    let observation = runner("success");
    assert_success(&observation);
    println!("L3 command-proof success=0 output=scrubbed entry=once");
}

#[test]
fn an_unrelated_failure_is_rejected() {
    let observation = runner("unrelated");
    let text = observation.combined();
    let diagnostic = observation.diagnostic("runner");
    assert!(
        !observation.status.success(),
        "wrong control failure\n{diagnostic}"
    );
    assert_scrubbed(&observation);
    assert!(
        text.contains("L3 command-child unrelated failure"),
        "wrong control failure\n{diagnostic}"
    );
    assert!(
        !text.contains("did not complete within its absolute work deadline"),
        "control unexpectedly expired\n{diagnostic}"
    );
    let rejected = std::panic::catch_unwind(|| assert_failure(&observation, "stall"));
    let payload = rejected.expect_err(&format!(
        "an unrelated failure was accepted as the regression\n{diagnostic}"
    ));
    assert!(
        panic_text(payload.as_ref()).contains("wrong deadline failure"),
        "unrelated rejection did not name wrong deadline failure\n{diagnostic}"
    );
    println!("L3 command-proof unrelated=rejected cause=wrong-deadline prerequisites=observed entry=once");
}

#[test]
fn native_command_runner_role() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    enter(RUNNER);
    let (mut command, entry) = test_command(CHILD);
    let child = command
        .env(MODE, &mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| {
            panic!(
                "start the owned banner command: {}; command status/output unavailable",
                scrub(&error.to_string())
            )
        });
    let pid = child.id();
    // Catch only to check this spawn's exact entry. Sanitation belongs to finish_bounded, before either hook runs.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finish_bounded(child, Duration::from_secs(5), &[])
    }));
    if let Err(payload) =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.assert_once(pid)))
    {
        let context = match &result {
            Ok(observation) => observation.diagnostic("command"),
            Err(payload) => panic_text(payload.as_ref()),
        };
        panic!(
            "child entry failed: {}\n{}",
            scrub(&panic_text(payload.as_ref())),
            scrub(&context)
        );
    }
    println!("L3 command-child entered=once");
    let observation = match result {
        Ok(observation) => observation,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    println!(
        "L3 command-finish exit={} output={}",
        observation.exit_record(),
        observation.combined()
    );
    assert!(
        observation.status.success(),
        "L3 HARNESS FAILURE: native fixture command failed\n{}",
        observation.diagnostic("command")
    );
}

#[test]
fn native_command_child_role() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    enter(CHILD);
    println!("On computer: {RAW_HOST}");
    println!("{RAW_PATH}/stdout-marker");
    eprintln!("{RAW_PATH}/stderr-marker");
    println!("L3 command-child banner=emitted paths=emitted");
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
    if mode == "owned" {
        println!("{}", std::env::var("SOT_L3_OWNED_IMAGE").unwrap());
        eprintln!("{}", std::env::var("SOT_L3_OWNED_MOUNT").unwrap());
        println!("L3 owned-child raw-image=emitted");
        eprintln!("L3 owned-child raw-mount=emitted");
        std::io::stdout().flush().unwrap();
        std::io::stderr().flush().unwrap();
    }
    if mode == "signal" {
        std::fs::write(std::env::var_os("SOT_L3_COMMAND_READY").unwrap(), b"ready").unwrap();
    }
    match mode.as_str() {
        "unrelated" => panic!("L3 command-child unrelated failure"),
        "stall" | "signal" => std::thread::sleep(Duration::from_secs(120)),
        "nonzero" => std::process::exit(7),
        "success" | "owned" => {}
        _ => panic!("unknown native command control"),
    }
}

fn owned_command() -> (
    std::process::Child,
    sot_log::test_isolated::Entry,
    tempfile::TempDir,
    String,
    String,
) {
    let dir = tempfile::tempdir_in(scratch()).unwrap();
    let image = dir.path().join("image.vhd").display().to_string();
    let mount = dir.path().join("mount").display().to_string();
    let (mut command, entry) = test_command(CHILD);
    let child = spawn_command(
        command
            .env(MODE, "owned")
            .env("SOT_L3_OWNED_IMAGE", &image)
            .env("SOT_L3_OWNED_MOUNT", &mount)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    (child, entry, dir, image, mount)
}

#[test]
fn native_owned_paths_are_scrubbed_before_general_roots() {
    let (child, entry, _dir, image, mount) = owned_command();
    let pid = child.id();
    let observation = finish_bounded(
        child,
        Duration::from_secs(5),
        &[(&image, "<owned-image>"), (&mount, "<owned-mount>")],
    );
    let diagnostic = observation.diagnostic("command");
    assert_entry(entry, pid, &observation);
    assert!(
        observation.status.success(),
        "owned command failed\n{diagnostic}"
    );
    assert!(
        observation
            .stdout
            .contains("L3 owned-child raw-image=emitted")
            && observation
                .stderr
                .contains("L3 owned-child raw-mount=emitted"),
        "owned path prerequisites missing\n{diagnostic}"
    );
    assert!(
        observation.stdout.contains("<owned-image>\n")
            && observation.stderr.contains("<owned-mount>\n")
            && !observation.combined().contains(&image)
            && !observation.combined().contains(&mount),
        "owned native paths were replaced after their roots\n{diagnostic}"
    );
    println!("L3 command-proof owned-paths=scrubbed-before-roots streams=separate entry=once");
}

#[test]
#[cfg(target_os = "macos")]
fn a_signal_native_command_keeps_its_status() {
    use std::os::unix::process::ExitStatusExt;
    let dir = tempfile::tempdir_in(scratch()).unwrap();
    let ready = dir.path().join("ready");
    let (mut command, entry) = test_command(CHILD);
    let mut child = spawn_command(
        command
            .env(MODE, "signal")
            .env("SOT_L3_COMMAND_READY", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let pid = child.id();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ready.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let entered = ready.exists();
    child
        .kill()
        .expect("end only the child returned by this spawn");
    // The child prints the synthetic root; this process scrubs it as an owned pair, since roots come from this
    // process's own environment.
    let observation = finish_bounded(child, Duration::from_secs(5), &[(RAW_PATH, "<target>")]);
    let diagnostic = observation.diagnostic("command");
    assert_entry(entry, pid, &observation);
    assert!(entered, "signal child readiness missing\n{diagnostic}");
    assert!(
        observation.status.signal() == Some(libc::SIGKILL) && observation.status.code().is_none(),
        "native command signal status was reduced to a sentinel\n{diagnostic}"
    );
    assert!(
        observation.stdout.contains("<target>/stdout-marker")
            && observation.stderr.contains("<target>/stderr-marker"),
        "signal streams missing\n{diagnostic}"
    );
    println!("L3 command-proof signal=retained code=none streams=separate entry=once");
}

fn assert_entry(entry: sot_log::test_isolated::Entry, pid: u32, observation: &NativeObservation) {
    if let Err(payload) =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.assert_once(pid)))
    {
        panic!(
            "command entry failed: {}\n{}",
            scrub(&panic_text(payload.as_ref())),
            observation.diagnostic("command")
        );
    }
}

fn spawn_command(command: &mut Command) -> std::process::Child {
    command.spawn().unwrap_or_else(|error| panic!(
        "start the owned command: {}; command status=unavailable; command stdout: unavailable; command stderr: unavailable",
        scrub(&error.to_string())
    ))
}
