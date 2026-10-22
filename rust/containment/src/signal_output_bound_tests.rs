use crate::leftover::Leftover;
use super::*;
use std::time::Duration;

#[test]
fn output_ends_at_its_bound() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let child = Leftover::of_file(pid_file.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(format!("echo $$ > '{}'\nexec sleep 3115", pid_file.display()));
        let began = std::time::Instant::now();
        let _ = tx.send((signal.output(&mut cmd, Duration::from_secs(1)).map(|o| o.status), began.elapsed()));
    });
    let (ended, took) = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| panic!("Signal::output had not returned after 5s"));
    let err = ended.expect_err("a child past its bound");
    println!("output-bound: {err} after {took:?}; the child is gone: {}", child.gone());
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(err.to_string(), "did not exit within 1s");
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert!(child.gone(), "the child past its bound survived");
    assert!(signal.held_groups().is_empty(), "the killed tree is still held");
}

/// A descendant that left the tree and keeps the pipe open does not hold the caller past the call's bound: the readers
/// share the deadline, and the one still open is left to end on its own with the escapee.
#[test]
fn output_does_not_wait_for_an_escaped_pipe_holder() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("perl");
        cmd.args(["-e", "use POSIX; my $p = fork; if (!$p) { POSIX::setsid(); exec 'sleep', '30' } print \"$p\\n\"; exit 0"]);
        let began = std::time::Instant::now();
        let _ = tx.send((signal.output(&mut cmd, Duration::from_secs(2)).map(|o| o.status), began.elapsed()));
    });
    let (ended, took) = rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("Signal::output was held past its bound by an escaped pipe holder"));
    let err = ended.expect_err("an escaped holder keeps the pipe open past the bound");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(took < Duration::from_secs(6), "{took:?}");
}
