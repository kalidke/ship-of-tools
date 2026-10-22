use super::*;
#[cfg(unix)]
use sot_containment::leftover::Leftover;

/// Program-path override per kernel project, so no test touches the process env.
pub(super) static STUB_BIN: std::sync::Mutex<Vec<(PathBuf, String)>> = std::sync::Mutex::new(Vec::new());

/// The request line the kernel child reads: fields in declaration order.
#[test]
fn wire_request_line_is_pinned() {
    let line = serde_json::to_string(&WireRequest {
        v: 1,
        id: 7,
        kind: "req",
        op: "x.y",
        payload: &serde_json::json!({"a": [1, "b"]}),
    })
    .unwrap();
    assert_eq!(line, r#"{"v":1,"id":7,"kind":"req","op":"x.y","payload":{"a":[1,"b"]}}"#);
}

/// The shutdown signal kills the kernel child and the loop neither
/// respawns nor leaves a registered tree.
#[cfg(unix)]
#[tokio::test]
async fn shutdown_kills_the_kernel_child_and_never_respawns() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("spawns");
    let stub = dir.path().join("stub-julia");
    sot_platform::test_exec::write_executable(&stub, format!("#!/bin/sh\necho x >> {}\nexec sleep 30\n", counter.display()));
    let project = dir.path().join("kp");
    std::fs::create_dir(&project).unwrap();
    STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let (status, _keep) = watch::channel(Status::Starting);
    let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
    let began = std::time::Instant::now();
    // Spawn completes before the stub's first line has run; fire only once
    // that line has written the counter, so the one-spawn precondition is true.
    while std::fs::read_to_string(&counter).map_or(0, |s| s.lines().count()) == 0 {
        assert!(began.elapsed() < Duration::from_secs(5), "the stub child never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!task.is_finished(), "the ready owner finished before the fire");
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("the supervisor loop outlived the shutdown")
        .expect("supervisor task");
    assert!(sig.held_groups().is_empty());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(), 1, "respawned after the fire");
}

/// The kernel's argv carries `--startup-file=no` before `-e`: it never loads the user's environment, and a startup file's
/// prints would land on its wire.
#[cfg(unix)]
#[tokio::test]
async fn the_kernel_spawn_passes_startup_file_no() {
    let dir = tempfile::tempdir().unwrap();
    let argv = dir.path().join("argv");
    let stub = dir.path().join("stub-julia");
    sot_platform::test_exec::write_executable(&stub, format!("#!/bin/sh\necho \"$@\" > {}\nexec sleep 30\n", argv.display()));
    let project = dir.path().join("kp");
    std::fs::create_dir(&project).unwrap();
    STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let (status, _keep) = watch::channel(Status::Starting);
    let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
    let began = std::time::Instant::now();
    while std::fs::read_to_string(&argv).map_or(true, |s| s.is_empty()) {
        assert!(began.elapsed() < Duration::from_secs(5), "the stub child never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let args = std::fs::read_to_string(&argv).unwrap();
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the supervisor loop outlived the shutdown").expect("supervisor task");
    // Before `-e`: after the script, Julia reads the flag as one of the script's arguments.
    let words: Vec<&str> = args.split_whitespace().collect();
    let flag = words.iter().position(|a| *a == "--startup-file=no");
    let script = words.iter().position(|a| *a == "-e");
    assert!(flag.is_some() && flag < script, "kernel argv: {args}");
}

/// A kernel that stops reading fills its pipe and the supervisor sits in
/// `write_request`, where it never polls the signal: the shutdown must
/// still take the kernel and what the kernel started.
#[cfg(unix)]
#[tokio::test]
async fn blocked_kernel_write_does_not_outlive_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let gc_file = dir.path().join("gc");
    let gc = Leftover::of_file(gc_file.clone());
    let stub = dir.path().join("stub-julia");
    sot_platform::test_exec::write_executable(
        &stub,
        format!(
            "#!/bin/sh\nsleep 3101 &\necho $! > {}\nread l\necho '{{\"id\":1,\"payload\":{{\"protocol\":0}}}}'\nexec sleep 3101\n",
            gc_file.display()
        ),
    );
    let project = dir.path().join("kp");
    std::fs::create_dir(&project).unwrap();
    STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let (status, mut status_rx) = watch::channel(Status::Starting);
    let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
    let tx = loop {
        let seen = status_rx.borrow_and_update().clone();
        if let Status::Running(tx) = seen {
            break tx;
        }
        tokio::time::timeout(Duration::from_secs(5), status_rx.changed())
            .await
            .expect("the stub kernel never answered hello")
            .expect("status channel");
    };
    // 256 KiB per request: the stub never reads again, so the first
    // write fills the pipe and the rest queue behind it.
    let big = "x".repeat(256 * 1024);
    let mut replies = Vec::new();
    for _ in 0..3 {
        let (reply, rx) = oneshot::channel();
        tx.send(Submission { op: "test.big".to_string(), payload: json!(big), reply }).await.unwrap();
        replies.push(rx);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(tx.capacity() < tx.max_capacity(), "the supervisor was not blocked on a full pipe");
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("the supervisor loop outlived the shutdown")
        .expect("supervisor task");
    assert!(gc.gone(), "the kernel's grandchild survived the shutdown");
    assert!(sig.held_groups().is_empty());
    drop(replies);
}

/// A kernel that answers hello and then dies is respawned on the doubling wait, 250 ms to 30 s, not every 250 ms:
/// an answered hello is not a working kernel (`supervisor_loop`, `Redial`). Each spawn appends a line; the doubling
/// spawns at 0, 0.25, 0.75 and 1.75 s, then not before 3.75 s, and the waits are lower bounds, so a slow machine can
/// only lower the count. Two spawns prove a respawn happened.
#[cfg(unix)]
#[tokio::test]
async fn a_kernel_that_answers_and_dies_is_respawned_on_the_doubling_wait() {
    const WINDOW: Duration = Duration::from_secs(3);
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("spawns");
    let stub = dir.path().join("stub-julia");
    sot_platform::test_exec::write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho x >> {}\nread l\necho '{{\"id\":1,\"payload\":{{\"protocol\":{KERNEL_PROTOCOL_VERSION}}}}}'\n",
            counter.display()
        ),
    );
    let project = dir.path().join("kp");
    std::fs::create_dir(&project).unwrap();
    STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let (status, _keep) = watch::channel(Status::Starting);
    let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
    tokio::time::sleep(WINDOW).await;
    let spawns = std::fs::read_to_string(&counter).map_or(0, |s| s.lines().count());
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("the supervisor loop outlived the shutdown")
        .expect("supervisor task");
    println!("respawn: {spawns} kernel spawns in {WINDOW:?} against a kernel that answers hello and exits");
    assert!(spawns >= 2, "the supervisor never respawned the kernel: {spawns} spawn(s) in {WINDOW:?}");
    assert!(spawns <= 4, "{spawns} kernel spawns in {WINDOW:?}: an answered hello restarted the wait");
}

/// A generation's temporary folder is removed once its child is ended and reaped. The stub stands in for the kernel
/// on the generation's own spawn path: it writes a file into its temporary folder, answers hello and waits. The
/// generation ends when its last handle drops, and then the owned folder that held the kernel's folder, in this
/// process's held folder of child folders, must be empty. `isolated` runs this in a process of its own, which gives
/// itself an owned temporary folder.
#[cfg(unix)]
#[tokio::test]
async fn a_generation_removes_its_temporary_folder_after_the_reap() {
    const BODY: Duration = Duration::from_secs(120);
    if !crate::sidecars::contract_tests::isolated(
        "sidecars::kernel::tests::a_generation_removes_its_temporary_folder_after_the_reap",
        BODY,
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("fixture folder");
    let owned = tempfile::tempdir().expect("owned temporary folder");
    std::env::set_var("TMPDIR", owned.path());
    let stub = dir.path().join("stub-julia");
    sot_platform::test_exec::write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho x > \"$TMPDIR/made\"\nread l\necho '{{\"id\":1,\"payload\":{{\"protocol\":{KERNEL_PROTOCOL_VERSION}}}}}'\nexec sleep 30\n"
        ),
    );
    let project = dir.path().join("kp");
    std::fs::create_dir(&project).unwrap();
    STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let (status, mut keep) = watch::channel(Status::Starting);
    let root = dir.path().to_path_buf();
    let generation = tokio::spawn(async move { run_one_generation(&project, &root, &status, sig, KernelBounds::PRODUCT).await });
    tokio::time::timeout(Duration::from_secs(30), keep.wait_for(|s| matches!(s, Status::Running(_))))
        .await
        .expect("the stub answers hello")
        .expect("status open");
    let listed = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    };
    let running: Vec<PathBuf> = listed(owned.path()).iter().flat_map(|held| listed(held)).filter(|p| p.is_dir()).collect();
    assert!(
        running.len() == 1 && running[0].join("made").exists(),
        "setup: the kernel's folder is in the held folder in the owned folder and holds the child's file: {running:?}"
    );
    drop(keep);
    let (_served, reason) = tokio::time::timeout(Duration::from_secs(30), generation)
        .await
        .expect("the generation ends when its handle drops")
        .expect("generation task");
    assert_eq!(reason, "owning kernel handle dropped");
    crate::sidecars::contract_tests::within(Duration::from_secs(30), "the kernel's folder is removed after its reap", || {
        listed(owned.path()).is_empty()
    })
    .await;
}

/// A kernel's working life starts at its hello. A precompile that runs past `STABLE` and then exits without
/// answering, or answers and dies at once, is not a working kernel: the next respawn waits the doubling, 2 s after
/// waits of 250 ms, 500 ms and 1 s, not the 250 ms floor. The paused clock moves only at the supervisor's waits and
/// at the test's one sleep, which stands for the long precompile, so the gap between the fourth and fifth spawn is
/// exact: that precompile plus the wait. A third case serves for `STABLE` after its hello and then dies: the next
/// spawn comes at the 250 ms floor. Each stub blocks until the test releases it, so every `Starting` is seen;
/// a watchdog thread fires the signal if a run stalls for a minute of real time.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_kernel_whose_precompile_outlasts_stable_without_serving_backs_off() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let precompile = sot_platform::host::redial::STABLE + Duration::from_secs(1);
    for (late_hello, served) in [(false, false), (true, false), (false, true)] {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("spawns");
        let stub = dir.path().join("stub-julia");
        sot_platform::test_exec::write_executable(
            &stub,
            format!(
                "#!/bin/sh\necho x >> {c}\nn=$(wc -l < {c} | tr -d ' ')\nwhile [ ! -f {d}/go-$n ]; do sleep 0.02; done\nif [ -f {d}/hello-$n ]; then read l; echo '{{\"id\":1,\"payload\":{{\"protocol\":{KERNEL_PROTOCOL_VERSION}}}}}'; fi\nif [ -f {d}/hold-$n ]; then while [ ! -f {d}/die-$n ]; do sleep 0.02; done; fi\n",
                c = counter.display(),
                d = dir.path().display(),
            ),
        );
        let project = dir.path().join("kp");
        std::fs::create_dir(&project).unwrap();
        STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
        let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
        let (status, mut status_rx) = watch::channel(Status::Starting);
        // No deadline: a paused clock jumps to any armed timer whenever the runtime waits on the stub, which
        // would end each generation at its hello bound and measure nothing here.
        let unbounded = KernelBounds { start: Duration::MAX, answer: Duration::MAX };
        let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, unbounded));
        let done = std::sync::Arc::new(AtomicBool::new(false));
        let watchdog = {
            let done = std::sync::Arc::clone(&done);
            std::thread::spawn(move || {
                for _ in 0..600 {
                    if done.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                sig.fire().expect("the watchdog fires");
            })
        };
        let mut starts = Vec::new();
        let mut died = None;
        while starts.len() < 5 {
            status_rx.changed().await.expect("the supervisor ended before its fifth spawn (the watchdog fired)");
            if !matches!(*status_rx.borrow_and_update(), Status::Starting) {
                continue;
            }
            starts.push(tokio::time::Instant::now());
            let n = starts.len();
            if n == 4 && served {
                // The kernel answers at once and serves for `STABLE` and a second, then dies.
                for f in ["hold-4", "hello-4", "go-4"] {
                    std::fs::write(dir.path().join(f), "").unwrap();
                }
                while !matches!(*status_rx.borrow_and_update(), Status::Running(_)) {
                    status_rx.changed().await.expect("the supervisor ended before the kernel answered hello");
                }
                tokio::time::sleep(precompile).await;
                died = Some(tokio::time::Instant::now());
                std::fs::write(dir.path().join("die-4"), "").unwrap();
            } else if n == 4 {
                tokio::time::sleep(precompile).await;
                if late_hello {
                    std::fs::write(dir.path().join("hello-4"), "").unwrap();
                }
            }
            if n < 5 {
                std::fs::write(dir.path().join(format!("go-{n}")), "").unwrap();
            }
        }
        done.store(true, Ordering::SeqCst);
        sig.fire().expect("fire");
        watchdog.join().unwrap();
        task.await.expect("supervisor task");
        if let Some(died) = died {
            let gap = starts[4] - died;
            println!("respawn: served for {precompile:?} after its hello: the spawn came {gap:?} after the kernel died");
            assert!(
                gap <= Duration::from_millis(300),
                "the spawn after a kernel that served {precompile:?} came {gap:?} after it died: the wait did not start over at the floor"
            );
            continue;
        }
        let gap = starts[4] - starts[3];
        println!("respawn: late_hello={late_hello}: the spawn after a {precompile:?} precompile came {gap:?} after it began");
        assert!(
            gap >= precompile + Duration::from_secs(2),
            "late_hello={late_hello}: the spawn after a {precompile:?} precompile came {gap:?} after it began: a kernel that did not serve counted as stable"
        );
    }
}

#[test]
fn kernel_unavailable_display_is_composable() {
    let dead = KernelUnavailable::Dead {
        reason: "julia exited at once (path /bin/false)".to_string(),
    };
    assert_eq!(
        format!("Julia kernel unavailable: {dead}"),
        "Julia kernel unavailable: julia exited at once (path /bin/false)"
    );
    let starting = KernelUnavailable::Starting { waited: Duration::from_secs(10) };
    assert!(format!("{starting}").contains("still starting"));
}

#[tokio::test]
async fn missing_kernel_project_reports_dead_without_spawning() {
    let dir = tempfile::tempdir().unwrap();
    let missing_project = dir.path().join("does-not-exist");
    // A stand-in program, never spawned: the check under test comes after the julia lookup, and a host with
    // no julia (a CI runner) fails that lookup first.
    STUB_BIN.lock().unwrap().push((missing_project.clone(), "never-spawned".to_string()));
    let kernel = Kernel::new(missing_project, dir.path().to_path_buf());
    let err = kernel.request("kernel.hello", json!({})).await.unwrap_err();
    let unavailable = err
        .downcast_ref::<KernelUnavailable>()
        .unwrap_or_else(|| panic!("expected KernelUnavailable, got: {err:#}"));
    match unavailable {
        KernelUnavailable::Dead { reason } => {
            assert!(reason.contains("kernel project missing"), "unexpected reason: {reason}");
        }
        other => panic!("expected Dead, got {other:?}"),
    }
}

/// The leak fix: once every `Kernel` handle drops, the supervisor task
/// must actually end (not run forever as an orphan holding its own
/// strong reference). Exercised at the `watch` level directly —
/// `supervisor_loop` itself is `async fn` and not otherwise observable
/// from outside this module without a real child process.
#[tokio::test]
async fn status_channel_reports_closed_once_every_kernel_handle_drops() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::new(dir.path().to_path_buf(), dir.path().to_path_buf());
    let status = kernel.inner.status.clone();
    assert!(!status.is_closed(), "keepalive receiver should hold it open");
    drop(kernel);
    assert!(status.is_closed(), "dropping the last Kernel handle should close status");
}

/// ADR 0049, User isolation: a kernel line that does not parse is logged by position and length, never its bytes.
#[test]
fn a_kernel_line_that_does_not_parse_is_logged_without_its_bytes() {
    let logged = crate::sidecars::logged_by(|| {
        let mut pending = HashMap::new();
        route_response(r#"{"v":1,"id":2,"url":"http://127.0.0.1:41234/0123456789ab"#, &mut pending);
    });
    assert_eq!(logged.lines().filter(|l| l.contains("WARN")).count(), 1, "{logged}");
    assert!(!logged.contains("0123") && !logged.contains("41234"), "{logged}");
}
