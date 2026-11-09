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
/// share the deadline, and the one still open is left to end on its own with the escapee. The escapee is this test's:
/// it holds the pipe while `keep` exists (its 30 s ceiling kept as a count), so the test ends it and sees it gone, and
/// no run of the suite leaves it behind.
#[test]
fn output_does_not_wait_for_an_escaped_pipe_holder() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let dir = tempfile::tempdir().unwrap();
    let (pid_file, keep) = (dir.path().join("escapee"), dir.path().join("keep"));
    std::fs::write(&keep, "").unwrap();
    let escapee = Leftover::of_file(pid_file.clone());
    let args = (pid_file, keep.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("perl");
        // The leader writes the escapee's pid before it exits, so the file is whole when the call returns.
        cmd.args([
            "-e",
            "use POSIX; my ($pid, $keep) = @ARGV; my $p = fork; \
             if (!$p) { POSIX::setsid(); for (1 .. 600) { last unless -e $keep; select(undef, undef, undef, 0.05) } POSIX::_exit(0) } \
             open(my $f, '>', $pid) or die; print $f \"$p\\n\"; close($f) or die; exit 0",
        ])
        .arg(&args.0)
        .arg(&args.1);
        let began = std::time::Instant::now();
        let _ = tx.send((signal.output(&mut cmd, Duration::from_secs(2)).map(|o| o.status), began.elapsed()));
    });
    let (ended, took) = rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("Signal::output was held past its bound by an escaped pipe holder"));
    // The escapee's identity is taken while it still holds the pipe, then it is released.
    let opened = escapee.pid();
    std::fs::remove_file(&keep).unwrap();
    assert!(opened.is_some(), "the leader wrote no escapee pid");
    let err = ended.expect_err("an escaped holder keeps the pipe open past the bound");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(took < Duration::from_secs(6), "{took:?}");
    assert!(escapee.gone(), "the escaped pipe holder outlived its test");
}
