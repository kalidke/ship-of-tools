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

fn runner(mode: &str) -> (bool, String) {
    let (mut command, entry) = test_command(RUNNER);
    let child = command
        .env(MODE, mode)
        .env("CARGO_TARGET_DIR", RAW_PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the owned native-command runner");
    let pid = child.id();
    let (status, out, err) = drain(child).wait_within(ISOLATION_TIMEOUT);
    entry.assert_once(pid);
    (status.success(), format!("{out}{err}"))
}

fn assert_scrubbed(text: &str) {
    assert!(
        text.contains("L3 command-child entered=once"),
        "child entry missing"
    );
    assert!(
        text.contains("L3 command-child banner=emitted paths=emitted"),
        "native command prerequisite missing"
    );
    assert!(
        text.contains("On computer: <host>"),
        "host replacement missing"
    );
    assert!(
        text.contains("<target>/stdout-marker"),
        "stdout path replacement missing"
    );
    assert!(
        text.contains("<target>/stderr-marker"),
        "stderr path replacement missing"
    );
    assert!(
        !text.contains(RAW_HOST),
        "raw synthetic host reached the panic hook"
    );
    assert!(
        !text.contains(RAW_PATH),
        "raw synthetic path reached the panic hook"
    );
}

fn assert_failure(success: bool, text: &str, mode: &str) {
    assert!(!success, "the entered failing runner succeeded");
    assert!(
        text.contains("L3 command-child entered=once"),
        "child entry missing"
    );
    assert!(
        text.contains("L3 command-child banner=emitted paths=emitted"),
        "native command prerequisite missing"
    );
    if mode == "stall" {
        assert!(
            text.contains("did not complete within its absolute work deadline"),
            "wrong deadline failure"
        );
        assert!(
            text.contains("cleanup=confirmed"),
            "owned-child cleanup unconfirmed"
        );
    } else {
        assert!(
            text.contains("L3 command-finish exit=7"),
            "wrong native exit failure"
        );
    }
    assert_scrubbed(text);
}

#[test]
fn a_native_command_failure_is_scrubbed_before_it_is_reported() {
    let (success, text) = runner("stall");
    assert_failure(success, &text, "stall");
    println!("L3 command-proof deadline=observed cleanup=confirmed hook=scrubbed entry=once");
}

#[test]
fn a_nonzero_native_command_failure_is_scrubbed() {
    let (success, text) = runner("nonzero");
    assert_failure(success, &text, "nonzero");
    println!("L3 command-proof nonzero=7 hook=scrubbed entry=once");
}

#[test]
fn successful_native_command_output_is_scrubbed() {
    let (success, text) = runner("success");
    assert!(success, "the entered successful runner failed");
    assert!(
        text.contains("L3 command-finish exit=0"),
        "success exit missing"
    );
    assert_scrubbed(&text);
    println!("L3 command-proof success=0 output=scrubbed entry=once");
}

#[test]
fn an_unrelated_failure_is_rejected() {
    let (success, text) = runner("unrelated");
    assert!(
        !success && text.contains("L3 command-child unrelated failure"),
        "wrong control failure"
    );
    assert!(
        text.contains("L3 command-child entered=once"),
        "child entry missing"
    );
    let rejected = std::panic::catch_unwind(|| assert_failure(success, &text, "stall"));
    assert!(
        rejected.is_err(),
        "an unrelated failure was accepted as the regression"
    );
    println!("L3 command-proof unrelated=rejected entry=once");
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
        .expect("start the owned banner command");
    let pid = child.id();
    // Catch only to check this spawn's exact entry. Sanitation belongs to finish_bounded, before either hook runs.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finish_bounded(child, Duration::from_secs(5))
    }));
    entry.assert_once(pid);
    println!("L3 command-child entered=once");
    let (code, text) = match result {
        Ok(observation) => observation,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    println!("L3 command-finish exit={code} output={text}");
    assert_eq!(
        code, 0,
        "L3 HARNESS FAILURE: native fixture command failed: {text}"
    );
}

#[test]
fn native_command_child_role() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    enter(CHILD);
    if mode == "unrelated" {
        panic!("L3 command-child unrelated failure");
    }
    println!("On computer: {RAW_HOST}");
    println!("{RAW_PATH}/stdout-marker");
    eprintln!("{RAW_PATH}/stderr-marker");
    println!("L3 command-child banner=emitted paths=emitted");
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
    match mode.as_str() {
        "stall" => std::thread::sleep(Duration::from_secs(120)),
        "nonzero" => std::process::exit(7),
        "success" => {}
        _ => panic!("unknown native command control"),
    }
}
