//! A guest daemon's `comm.file` forward to its folder's hub, over topology's one-shot dial, bounded by a deadline and by
//! the shutdown signal.

use crate::topology::dial::{dial_and_call_tracked, Track};

/// A guest daemon's `comm.file` forward to its folder's hub (0031 B1): the
/// request as given, the hub's payload verbatim. [`dial_and_call`] sets no
/// deadline of its own, so the call runs on its own thread and no answer
/// within `within` is an error, and so is the shutdown signal firing first;
/// either way the ssh child is killed before this returns, and the thread
/// ends with the connection.
pub fn forward_comm_file(
    endpoint: &str,
    self_host: &str,
    req: &sot_protocol::CommFileReq,
    within: std::time::Duration,
    sig: &'static crate::lifecycle::child_signal::Signal,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::to_value(req).map_err(|e| e.to_string())?;
    let (tx, rx) = std::sync::mpsc::channel();
    let (e, h) = (endpoint.to_string(), self_host.to_string());
    let track = std::sync::Arc::new(Track::new(sig));
    let dial_track = std::sync::Arc::clone(&track);
    std::thread::spawn(move || {
        let _ = tx.send(dial_and_call_tracked(&e, &h, sot_protocol::op::COMM_FILE, payload, Some(dial_track)));
    });
    let deadline = std::time::Instant::now() + within;
    loop {
        let step = deadline.saturating_duration_since(std::time::Instant::now()).min(std::time::Duration::from_millis(50));
        match rx.recv_timeout(step) {
            Ok(done) => return done,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(format!("{endpoint}: the forward thread ended")),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if sig.is_fired() {
            track.cancel();
            return Err(format!("{endpoint}: the daemon is shutting down"));
        }
        if std::time::Instant::now() >= deadline {
            track.cancel();
            return Err(format!("{endpoint}: no reply to {} within {}s", sot_protocol::op::COMM_FILE, within.as_secs()));
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::topology::dial::tests::{prepend_to_path, wait_for_marker, write_ssh_script, EnvGuard};

    /// A forwarded comm.file call is cut short by its own timeout or by the
    /// shutdown signal, and either way the ssh child dies and is no longer counted.
    #[cfg(unix)]
    #[test]
    fn forward_comm_file_kills_its_ssh_child_on_timeout_and_on_shutdown() {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _path_guard = EnvGuard::capture("PATH");
        let req: sot_protocol::CommFileReq = serde_json::from_value(serde_json::json!({"from": "a", "to": "b", "text": "t"})).expect("a CommFileReq");
        let alive = |pid: i32| unsafe { libc::kill(pid, 0) == 0 };
        for by_shutdown in [false, true] {
            let dir = tempfile::tempdir().expect("tempdir");
            let pid_file = dir.path().join("pid");
            write_ssh_script(dir.path(), &format!("echo $$ > '{}'\nexec sleep 30\n", pid_file.display()));
            prepend_to_path(dir.path());
            let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
            let within = std::time::Duration::from_secs(if by_shutdown { 20 } else { 1 });
            let firer = std::thread::spawn({
                let pid_file = pid_file.clone();
                move || {
                    if by_shutdown {
                        assert!(wait_for_marker(&pid_file));
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        sig.fire();
                    }
                }
            });
            let began = std::time::Instant::now();
            let result = forward_comm_file("ssh:hub", "self", &req, within, sig);
            firer.join().unwrap();
            assert!(result.is_err());
            assert!(began.elapsed() < std::time::Duration::from_secs(5), "the forward outlived its bound");
            let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
            let gone = (0..100).any(|_| {
                std::thread::sleep(std::time::Duration::from_millis(30));
                !alive(pid) && sig.live() == 0
            });
            assert!(gone, "the ssh child survived (shutdown={by_shutdown}) or is still counted");
        }
    }
}
