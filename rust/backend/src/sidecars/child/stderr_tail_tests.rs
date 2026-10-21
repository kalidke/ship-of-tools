use super::{StderrTail, STARTING_LINES, TAIL_LINES};
use crate::lifecycle::child_signal::Signal;
use std::process::Stdio;

/// Ends a `/bin/sh -c script` child and logs its tail under `what`.
async fn run(sig: &'static Signal, script: &str, what: &str) {
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.args(["-c", script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = sig.spawn(&mut cmd).unwrap();
    let tail = StderrTail::drain(child.stderr.take().unwrap(), |_| {});
    child.wait().await.unwrap();
    tail.warn(what).await;
}

/// The last `TAIL_LINES` lines reach the log at warn, the earlier ones do not, and a child that wrote none logs
/// nothing.
#[tokio::test]
async fn a_child_s_last_lines_are_logged_at_warn() {
    let log = sot_log::test_log::capture();
    let warned = |needle: &str| crate::sidecars::warned(&log.text(), needle);
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    run(sig, "i=0; while [ $i -lt 40 ]; do echo \"line-$i.\" >&2; i=$((i + 1)); done", "the child ended").await;
    let first_kept = 40 - TAIL_LINES;
    assert!(warned("the child ended"), "the tail was not logged at warn: {}", log.text());
    assert!(warned(&format!("line-{first_kept}.")) && warned("line-39."), "the last lines are kept");
    assert!(!warned(&format!("line-{}.", first_kept - 1)), "an earlier line was kept");
    run(sig, "exit 1", "the quiet child ended").await;
    assert!(!warned("the quiet child ended"), "a child with no stderr logged a tail");
}

/// A child still starting is named with its age and its last `STARTING_LINES` lines that hold text, oldest first
/// and trimmed, while it runs; a child that wrote nothing says so.
#[tokio::test]
async fn a_starting_child_is_named_with_its_age_and_last_lines() {
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    let starting = |script: &str| {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = sig.spawn(&mut cmd).unwrap();
        (StderrTail::drain(child.stderr.take().unwrap(), |_| {}).start(), child)
    };
    let (quiet, _quiet) = starting("exec sleep 30");
    let nothing = "the quiet child is still starting after 0 s; it has written nothing to stderr";
    assert_eq!(quiet.still_starting("the quiet child"), nothing);
    let script = "for l in one two '' '   three   ' four five six; do echo \"$l\" >&2; done; exec sleep 30";
    let (start, _child) = starting(script);
    assert_eq!(STARTING_LINES, 5);
    let lines = " s; its last stderr lines: two / three / four / five / six";
    crate::sidecars::contract_tests::within(std::time::Duration::from_secs(5), lines, || {
        start.still_starting("the child").ends_with(lines)
    })
    .await;
    let said = start.still_starting("the child");
    let age = said.strip_prefix("the child is still starting after ").and_then(|rest| rest.strip_suffix(lines));
    assert!(age.and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n < 5), "{said}");
    sig.fire().expect("fire");
}
