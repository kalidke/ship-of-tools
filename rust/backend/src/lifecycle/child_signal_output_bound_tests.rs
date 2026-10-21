use super::tests::Leftover;
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
