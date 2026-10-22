#![cfg(unix)]
//! `sotd topology sync`: the real binary with a stand-in `ssh` first on PATH that never answers, so the sync's own
//! bound is what ends it.
use std::time::{Duration, Instant};

#[path = "support/sotd.rs"]
mod sotd;

/// Seconds the test waits for the sync to end: past the sync's 30 s bound and its ssh's own.
const WAIT: u64 = 45;

#[test]
fn a_sync_whose_ssh_never_answers_ends_at_its_bound() {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    sot_platform::test_exec::write_executable(&bin.join("ssh"), "#!/bin/sh\nexec sleep 600\n");
    let mut cmd = sotd::sotd_command();
    let mut child = cmd
        .env_clear()
        .args(["topology", "sync", "--hub", "stubhub"])
        .env("HOME", t.path())
        .env("XDG_CONFIG_HOME", t.path().join("config"))
        .env("SOT_SELF_HOST", "sync-box")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let began = Instant::now();
    // The red's wait is the old code's: a sync that no bound ends is killed here, its own child.
    let limit = Duration::from_secs(WAIT);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if began.elapsed() > limit {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("topology sync had not exited after {WAIT}s");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let took = began.elapsed();
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    println!("sync-bound: {status} after {took:?}: {}", stderr.trim());
    assert!(!status.success(), "a sync whose ssh never answered succeeded");
    assert!(stderr.contains("ssh stubhub: did not exit within 30s"), "{stderr}");
}
