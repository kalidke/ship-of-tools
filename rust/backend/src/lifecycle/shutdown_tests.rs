use super::*;
use crate::rows::run::end_run::EndRunOutcome as O;

/// A child creation stalled in the OS holds the registry mutex the fire needs: the terminal hands its code on after the
/// wait, and the fire completes when the creation does.
#[cfg(unix)]
#[test]
fn the_terminal_does_not_wait_for_a_stalled_creation() {
    use crate::lifecycle::child_signal::Signal;
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
    let code = terminal(signal, 7, |code| code);
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
    use crate::lifecycle::child_signal::Signal;
    use std::sync::{mpsc, Barrier};
    let name = "lifecycle::shutdown::tests::a_terminal_with_no_thread_to_fire_on_exits_without_the_fire";
    if !sot_log::test_isolated::run_isolated(name) {
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
    let code = terminal(signal, 7, |code| code);
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
    use crate::lifecycle::child_signal::Signal;
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let handed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (first_in, first_inside) = std::sync::mpsc::channel();
    let record = handed.clone();
    let first = std::thread::spawn(move || {
        terminal(signal, 1, |code| {
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
        terminal(signal, 0, |code| record.lock().unwrap().push(code))
    });
    std::thread::sleep(Duration::from_millis(1000));
    first.join().unwrap();
    assert_eq!(
        *handed.lock().unwrap(),
        vec![1],
        "the second controlled exit reached the process exit"
    );
}

#[tokio::test(start_paused = true)]
async fn end_with_retry_table() {
    // Kept twice (starting), then a confirmed end: ended.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut script = vec![O::NotEnded("starting".into()), O::NotEnded("starting".into()), O::RecordVerified].into_iter();
    let got = retry_until(deadline, || {
        let o = script.next().expect("no attempt after a confirmed end");
        async move { confirmed(crate::rows::run::end::capsule_destroy_outcome_of(o)) }
    })
    .await;
    assert_eq!(got, Ok(true));

    // Refused forever: tried once per second, then not ended at the deadline.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut tries = 0;
    let got = retry_until(deadline, || {
        tries += 1;
        async { confirmed(crate::rows::run::end::capsule_destroy_outcome_of(O::NotEnded("refused".into()))) }
    })
    .await;
    assert_eq!(got, Err("refused".to_string()));
    assert_eq!(tries, 10, "one attempt per second across the 10 s");
    assert!(Instant::now() + RETRY_EVERY >= deadline, "gave up before the deadline");

    // An attempt still running at the deadline: abandoned, not ended.
    let deadline = Instant::now() + Duration::from_secs(10);
    let got = retry_until(deadline, || std::future::pending::<Result<(), String>>()).await;
    assert!(got.is_err());
    assert_eq!(Instant::now(), deadline);
}

#[test]
fn drawer_is_an_end_target() {
    let root = tempfile::tempdir().expect("tempdir");
    assert!(!drawer_is_target(root.path()), "no pointer, no drawer to end");

    std::fs::write(sot_log::supervisor::journal::pointer::pointer_path(root.path()), "x").expect("pointer");
    assert!(drawer_is_target(root.path()), "a drawer pointer makes the drawer an end target");
}

#[tokio::test(start_paused = true)]
async fn forget_on_unremovable_registration() {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut tries = 0;
    let ended = forget_unless_removed("stuck".into(), deadline, || {
        tries += 1;
        false
    })
    .await;
    assert_eq!(ended, Ended::Forget("stuck".into()));
    assert_eq!(tries, 5, "retried until the deadline");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut tries = 0;
    let ended = forget_unless_removed("late".into(), deadline, || {
        tries += 1;
        tries == 3
    })
    .await;
    assert_eq!(ended, Ended::Row("late".into()), "removed on a retry is not forgotten");
}

#[tokio::test(start_paused = true)]
async fn end_rows_returns_by_the_shared_deadline() {
    let deadline = Instant::now() + Duration::from_secs(5);
    let joins = vec![
        ("stuck".to_string(), tokio::spawn(std::future::pending::<Ended>())),
        ("done".to_string(), tokio::spawn(async { Ended::Row("done".into()) })),
    ];
    let report = tokio::time::timeout_at(deadline + Duration::from_secs(1), join_by(deadline, joins))
        .await
        .expect("end_rows did not return by its deadline");
    assert_eq!(report, EndReport { ended: vec!["done".into()], not_ended: 1, forget: Vec::new() });
}

#[tokio::test]
async fn non_capsule_row_is_counted_not_ended() {
    let root = tempfile::tempdir().expect("tempdir");
    let reg = Workspaces::new();
    let mut row = Workspace::from_label("tm", PathBuf::from("/p/tm"), false, "none".into(), String::new(), String::new());
    row.runtime = "tmux".to_string();
    reg.insert(row);
    let (events, _rx) = broadcast::channel(4);
    let report = end_rows(reg.list(), &reg, &events, root.path(), Instant::now() + Duration::from_secs(3)).await;
    assert_eq!(report.not_ended, 1, "a row the shutdown cannot end was not counted");
}

#[tokio::test]
async fn already_removed_row_is_not_kept() {
    let reg = Workspaces::new();
    let mut row = Workspace::from_label("gone", PathBuf::from("/p/gone"), false, "none".into(), String::new(), String::new());
    row.runtime = "capsule".to_string();
    let row = reg.insert(row);
    // Another end removed it first.
    let _ = reg.remove_by_id(&row.workspace_id);
    let (events, _rx) = broadcast::channel(4);
    let started = Instant::now();
    let ended = end_row(&row, false, &reg, &events, started + Duration::from_secs(3)).await;
    assert_eq!(ended, Ended::Gone);
    assert!(started.elapsed() < RETRY_EVERY, "an already-removed row was retried");
}
