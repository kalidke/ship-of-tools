use super::*;

/// A child creation stalled in the OS holds the registry mutex the fire needs: the terminal hands its code on after the
/// wait, and the fire completes when the creation does.
#[cfg(unix)]
#[test]
fn the_terminal_does_not_wait_for_a_stalled_creation() {
    use sot_containment::Signal;
    use std::sync::{mpsc, Barrier};
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let released = Arc::new(Barrier::new(2));
    let (entered, arrived) = mpsc::channel();
    let hook_released = released.clone();
    *signal.after_create.lock().unwrap() = Some(Box::new(move |_| {
        entered.send(()).unwrap();
        hook_released.wait();
    }));
    let creation = std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("600");
        if let Ok(mut child) = signal.spawn_std(&mut cmd) {
            let _ = child.kill();
        }
    });
    arrived
        .recv_timeout(Duration::from_secs(10))
        .expect("the creation did not stall");
    let began = std::time::Instant::now();
    let code = terminal(signal, 7, |_| {}, |code| code);
    let waited = began.elapsed();
    released.wait();
    creation.join().unwrap();
    assert_eq!(code, 7);
    assert!(
        waited < Duration::from_secs(5),
        "the terminal waited {waited:?} for a stalled creation"
    );
    assert!(
        signal.is_fired(),
        "the permanent flag was not published before the wait"
    );
}

/// A terminal that cannot start its fire thread does not fire on its own thread either: a stalled creation holds the
/// registry mutex the fire needs, and this caller holds the exit's claim, so a fire here would park every exit. It hands
/// its code on at once. The thread limit is this isolated child's own; a privileged account, which no limit stops, is a
/// setup failure.
#[cfg(target_os = "linux")]
#[test]
fn a_terminal_with_no_thread_to_fire_on_exits_without_the_fire() {
    use sot_containment::Signal;
    use std::sync::{mpsc, Barrier};
    let name = "lifecycle::shutdown::tests::a_terminal_with_no_thread_to_fire_on_exits_without_the_fire";
    if !sot_platform::test_isolated::run_isolated(name) {
        return;
    }
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let released = Arc::new(Barrier::new(2));
    let (entered, arrived) = mpsc::channel();
    let hook_released = released.clone();
    *signal.after_create.lock().unwrap() = Some(Box::new(move |_| {
        entered.send(()).unwrap();
        hook_released.wait();
    }));
    let creation = std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("600");
        if let Ok(mut child) = signal.spawn_std(&mut cmd) {
            let _ = child.kill();
        }
    });
    arrived
        .recv_timeout(Duration::from_secs(10))
        .expect("the creation did not stall");
    let mut saved = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: reading and lowering this process's own soft thread limit into and from local values.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NPROC, &mut saved), 0);
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: saved.rlim_max,
        };
        assert_eq!(
            libc::setrlimit(libc::RLIMIT_NPROC, &none),
            0,
            "setup: lower the thread limit"
        );
    }
    assert!(
        std::thread::Builder::new().spawn(|| {}).is_err(),
        "setup: a thread can still be started (a privileged account?)"
    );
    let began = std::time::Instant::now();
    let code = terminal(signal, 7, |_| {}, |code| code);
    let waited = began.elapsed();
    // SAFETY: restoring the limit read above.
    unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &saved) };
    released.wait();
    creation.join().unwrap();
    assert_eq!(code, 7);
    assert!(
        waited < Duration::from_secs(5),
        "the terminal with no fire thread waited {waited:?} for a stalled creation"
    );
}

/// Two controlled ends that arrive together (the backstop and a close finishing at the bound): the first one's code is
/// the only one handed to the exit, and the second never reaches it.
#[test]
fn the_first_controlled_exit_decides_the_code() {
    use sot_containment::Signal;
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let handed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (first_in, first_inside) = std::sync::mpsc::channel();
    let record = handed.clone();
    let first = std::thread::spawn(move || {
        terminal(signal, 1, |_| {}, |code| {
            record.lock().unwrap().push(code);
            first_in.send(()).unwrap();
            // The first exit is still on its way out when the second arrives.
            std::thread::sleep(Duration::from_millis(500));
        })
    });
    first_inside
        .recv_timeout(Duration::from_secs(10))
        .expect("the first exit did not reach its end");
    let record = handed.clone();
    let _second = std::thread::spawn(move || {
        terminal(signal, 0, |_| {}, |code| record.lock().unwrap().push(code))
    });
    std::thread::sleep(Duration::from_millis(1000));
    first.join().unwrap();
    assert_eq!(
        *handed.lock().unwrap(),
        vec![1],
        "the second controlled exit reached the process exit"
    );
}

/// The terminal fires before it removes the children's folders, hands that removal no more than `FOLDERS_WAIT`, and
/// removes them before the exit takes the code.
#[test]
fn the_terminal_removes_the_folders_within_their_wait_before_it_exits() {
    use sot_containment::Signal;
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let steps = std::sync::Mutex::new(Vec::new());
    let code = terminal(
        signal,
        3,
        |wait| steps.lock().unwrap().push(("folders", wait, signal.is_fired())),
        |code| {
            steps.lock().unwrap().push(("exit", Duration::ZERO, signal.is_fired()));
            code
        },
    );
    assert_eq!(code, 3);
    assert_eq!(
        *steps.lock().unwrap(),
        vec![("folders", FOLDERS_WAIT, true), ("exit", Duration::ZERO, true)],
        "the folders were not removed after the fire, within their wait, and before the exit"
    );
}
