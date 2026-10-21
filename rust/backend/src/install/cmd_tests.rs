use super::*;

/// A stop ends the running step and every process it started before it returns, so nothing of the step writes after
/// the verdict; a step that ignores the signal is killed after the grace.
#[cfg(unix)]
#[test]
fn a_stop_ends_the_running_step_and_what_it_started() {
    for script in ["sleep 30 & echo $! > \"$1\"; wait", "trap '' TERM; sleep 30 & echo $! > \"$1\"; wait; wait"] {
        let steps: &'static Steps = Box::leak(Box::new(Steps::new()));
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild");
        let p = pidfile.clone();
        let body = script.to_string();
        let step = std::thread::spawn(move || steps.output(Command::new("sh").arg("-c").arg(&body).arg("sh").arg(&p)));
        let t0 = Instant::now();
        while !pidfile.is_file() && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let grandchild: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        let t1 = Instant::now();
        steps.stop(libc::SIGTERM, Duration::from_secs(2));
        assert!(t1.elapsed() < Duration::from_secs(4), "{script}: the stop took {:?}", t1.elapsed());
        let ran = step.join().unwrap();
        assert_ne!(ran.code, Some(0), "{script}");
        let gone = (0..100).any(|_| {
            // SAFETY: a probe of a pid this test's step started; signal 0 sends nothing.
            let alive = unsafe { libc::kill(grandchild, 0) } == 0;
            if alive {
                std::thread::sleep(Duration::from_millis(20));
            }
            !alive
        });
        assert!(gone, "{script}: the step's own child outlived the stop");
        assert!(steps.is_stopping());
    }
}

/// A bounded step that outlives its bound is ended at it, with what it started, by the clock.
#[cfg(unix)]
#[test]
fn a_bounded_step_ends_at_its_bound() {
    let steps = Steps::new();
    let t0 = Instant::now();
    let ran = steps.output_within(Command::new("sh").args(["-c", "trap '' TERM; sleep 30"]), Duration::from_millis(300));
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    assert!(ran.code.is_none() && ran.err.contains("did not finish within 300 ms"), "{}", ran.err);
}
