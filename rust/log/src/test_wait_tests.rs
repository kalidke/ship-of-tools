use super::*;

const BOUND: Duration = Duration::from_secs(30);

/// A shell running `script`: the child every case below runs.
fn shell(script: &str) -> Command {
    let (program, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
    let mut command = Command::new(program);
    command.args([flag, script]);
    command
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|text| (*text).to_string()))
        .unwrap_or_default()
}

fn fails(check: impl FnOnce()) -> String {
    panic_text(std::panic::catch_unwind(std::panic::AssertUnwindSafe(check)).expect_err("a check that must fail passed"))
}

/// Every way a child can fail a test names its command, its end and both streams; a clean run is returned whole.
#[test]
fn every_way_a_child_fails_names_its_command_status_and_streams() {
    #[cfg(unix)]
    let (said, failed, flood) = (
        "echo said-on-stdout; echo said-on-stderr >&2",
        "echo said-on-stdout; echo said-on-stderr >&2; exit 3",
        "i=0; while [ $i -lt 2000 ]; do echo 0123456789abcdef0123456789abcdef; i=$((i+1)); done; exit 4",
    );
    #[cfg(windows)]
    let (said, failed, flood) = (
        "echo said-on-stdout& echo said-on-stderr 1>&2",
        "echo said-on-stdout& echo said-on-stderr 1>&2& exit /b 3",
        "(for /L %i in (1,1,2000) do @echo 0123456789abcdef0123456789abcdef)& exit /b 4",
    );
    let ran = run_within(&mut shell(said), BOUND).ok();
    assert!(ran.stdout.contains("said-on-stdout") && ran.stderr.contains("said-on-stderr"), "{ran}");

    let text = fails(|| {
        run_within(&mut shell(failed), BOUND).ok();
    });
    for want in ["said-on-stdout", "said-on-stderr", "3", "--- stdout ---", "--- stderr ---", failed] {
        assert!(text.contains(want), "{want:?} missing from: {text}");
    }

    let text = fails(|| {
        run_within(&mut shell(flood), BOUND).ok();
    });
    assert!(text.contains(&format!("last {FAILURE_STREAM_BYTES} of ")), "the flood was not cut: {text}");
    assert!(text.len() < 3 * FAILURE_STREAM_BYTES, "the failure carries {} bytes", text.len());
}

/// A child that dies by a signal says so, which an exit code alone cannot.
#[cfg(unix)]
#[test]
fn a_child_killed_by_a_signal_names_the_signal() {
    let text = fails(|| {
        run_within(&mut shell("echo before-the-kill; kill -9 $$"), BOUND).ok();
    });
    assert!(text.contains("signal: 9") && text.contains("before-the-kill"), "{text}");
}

/// A child still running at its bound is killed and has no account: the reason names its command and what it had
/// said, and `try_run_within` returns it without a panic.
#[test]
fn a_child_that_overruns_its_bound_fails_by_name() {
    #[cfg(unix)]
    let stall = "echo before-the-stall; exec sleep 60";
    #[cfg(windows)]
    let stall = "echo before-the-stall& ping -n 60 127.0.0.1 >NUL";
    let started = std::time::Instant::now();
    let text = match try_run_within(&mut shell(stall), Duration::from_secs(2)) {
        Ok(ran) => panic!("a stalled child returned an account: {ran}"),
        Err(text) => text,
    };
    assert!(text.contains("did not complete within its absolute work deadline"), "{text}");
    assert!(text.contains(stall) && text.contains("before-the-stall"), "{text}");
    assert!(started.elapsed() < Duration::from_secs(20), "the bound was not kept");
}

/// A child that cannot start is named without a panic by `try_run_within`, and fails the caller of `run_within`
/// with the same words.
#[test]
fn a_child_that_cannot_start_is_named() {
    const MISSING: &str = "sot-test-wait-no-such-program";
    let text = match try_run_within(&mut Command::new(MISSING), BOUND) {
        Ok(ran) => panic!("a missing program ran: {ran}"),
        Err(text) => text,
    };
    assert!(text.starts_with(&format!("start {MISSING}: ")), "{text}");
    assert_eq!(fails(|| drop(run_within(&mut Command::new(MISSING), BOUND))), text);
}

/// `within` returns what its wait returns, re-raises the wait's own panic, and fails a wait that overruns by name.
#[test]
fn within_returns_reraises_and_names_an_overrun() {
    assert_eq!(within(BOUND, "a quick wait", || 7), 7);
    let text = fails(|| {
        within(BOUND, "a panicking wait", || panic!("the wait's own panic"));
    });
    assert_eq!(text, "the wait's own panic");
    let (_keep, held) = std::sync::mpsc::channel::<()>();
    let started = std::time::Instant::now();
    let text = fails(|| {
        let _ = within(Duration::from_millis(200), "the held receive", move || held.recv());
    });
    assert!(text.contains("the held receive did not end within 200ms"), "{text}");
    assert!(started.elapsed() < Duration::from_secs(5), "the bound was not kept");
}
