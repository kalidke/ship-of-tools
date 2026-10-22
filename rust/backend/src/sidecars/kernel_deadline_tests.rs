use super::*;

/// The lines of a stand-in kernel that read the hello and answer it.
pub(super) fn hello_answer() -> String {
    format!("read l\necho '{{\"id\":1,\"payload\":{{\"protocol\":{KERNEL_PROTOCOL_VERSION}}}}}'\n")
}

/// A kernel project whose stand-in appends a line to the returned counter at each spawn and then runs `script`.
pub(super) fn stub_kernel(dir: &Path, name: &str, script: &str) -> (PathBuf, PathBuf) {
    let counter = dir.join(format!("{name}-spawns"));
    let stub = dir.join(format!("{name}-julia"));
    sot_platform::test_exec::write_executable(&stub, format!("#!/bin/sh\necho x >> {}\n{script}", counter.display()));
    let project = dir.join(name);
    std::fs::create_dir(&project).unwrap();
    tests::STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
    (project, counter)
}

/// A kernel project whose stand-in answers its hello (or not) and then reads and answers nothing.
fn wedged_kernel(dir: &Path, name: &str, hello: bool) -> (PathBuf, PathBuf) {
    let answer = if hello { hello_answer() } else { String::new() };
    stub_kernel(dir, name, &format!("{answer}exec sleep 600\n"))
}

/// The running kernel's submission channel, once its stand-in has answered the hello (each wait within 5 s).
pub(super) async fn running(status_rx: &mut watch::Receiver<Status>) -> mpsc::Sender<Submission> {
    loop {
        if let Status::Running(tx) = status_rx.borrow_and_update().clone() {
            return tx;
        }
        tokio::time::timeout(Duration::from_secs(5), status_rx.changed())
            .await
            .expect("the stub kernel never answered hello")
            .expect("status channel");
    }
}

pub(super) fn spawns(counter: &Path) -> usize {
    std::fs::read_to_string(counter).map_or(0, |s| s.lines().count())
}

/// Polls `condition` for up to `bound`.
async fn becomes(bound: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + bound;
    while !condition() {
        if Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    true
}

/// A caller's timeout on a running kernel does not say the kernel is starting, and a kernel that answered its
/// hello and then nothing is ended and started again once it has answered nothing for the answer bound.
#[tokio::test]
async fn a_wedged_kernel_is_named_and_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (project, counter) = wedged_kernel(dir.path(), "named", true);
    let kernel = Kernel::new(project, dir.path().to_path_buf());
    let err = kernel.request("project.scan", json!({})).await.expect_err("a kernel that answers nothing");
    let named = format!("{err:#}");
    drop(kernel);

    let (project, counter2) = wedged_kernel(dir.path(), "replaced", true);
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let (status, mut status_rx) = watch::channel(Status::Starting);
    let bounds = KernelBounds { start: Duration::from_secs(1800), answer: Duration::from_secs(2) };
    let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, bounds));
    let tx = running(&mut status_rx).await;
    let reason = submit_and_await(&tx, "project.scan", json!({}), KERNEL_REQUEST_TIMEOUT).await.expect_err("a kernel that answers nothing");
    let replaced = becomes(Duration::from_secs(15), || spawns(&counter2) >= 2).await;
    println!("kernel-answer: {reason:#}; spawned {} times", spawns(&counter2));
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the supervisor loop outlived the shutdown").expect("task");
    assert_eq!(spawns(&counter), 1);
    assert!(!named.contains("still starting"), "a running kernel's silence is named as starting: {named}");
    assert!(named.contains("the running kernel did not answer project.scan within 10s"), "{named}");
    assert!(replaced, "a kernel that answered nothing was never replaced: spawned {} time(s) in 15 s", spawns(&counter2));
    assert!(format!("{reason:#}").contains("answered nothing for 2s (oldest: project.scan)"), "{reason:#}");
}

/// A kernel that answers every request, each 2 s after the one before, is kept under a 3 s answer bound: the bound
/// counts from the later of the oldest request's write and the kernel's last answer, so the time a request waits
/// behind slow answers is not the kernel's silence.
#[tokio::test]
async fn a_kernel_answering_slowly_but_steadily_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let steady = r#"while read l; do sleep 2; id=$(printf '%s\n' "$l" | sed 's/.*"id":\([0-9]*\).*/\1/'); echo "{\"id\":$id,\"payload\":{}}"; done
"#;
    let (project, counter) = stub_kernel(dir.path(), "steady", &format!("{}{steady}", hello_answer()));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let (status, mut status_rx) = watch::channel(Status::Starting);
    let bounds = KernelBounds { start: Duration::from_secs(1800), answer: Duration::from_secs(3) };
    let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, bounds));
    let tx = running(&mut status_rx).await;
    let calls: Vec<_> = (1..=3)
        .map(|n| {
            let tx = tx.clone();
            tokio::spawn(async move { (n, submit_and_await(&tx, "test.slow", json!({}), Duration::from_secs(15)).await) })
        })
        .collect();
    let mut failed = Vec::new();
    for call in calls {
        let (n, answer) = call.await.expect("request task");
        if let Err(e) = answer {
            failed.push(format!("request {n}: {e:#}"));
        }
    }
    println!("kernel-steady: failed {failed:?}; spawned {} time(s)", spawns(&counter));
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the supervisor loop outlived the shutdown").expect("task");
    assert!(failed.is_empty(), "a kernel answering every 2 s was cut: {failed:?}");
    assert_eq!(spawns(&counter), 1, "a kernel answering every 2 s was replaced");
}

/// A kernel that never answers its hello within `start` is ended and replaced, and the reason names the hello.
#[tokio::test]
async fn a_kernel_that_never_says_hello_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (project, counter) = wedged_kernel(dir.path(), "mute", false);
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let (status, mut status_rx) = watch::channel(Status::Starting);
    let bounds = KernelBounds { start: Duration::from_secs(1), answer: Duration::from_secs(300) };
    let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, bounds));
    let reason = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            status_rx.changed().await.expect("status channel");
            if let Status::Dead { reason } = &*status_rx.borrow_and_update() {
                break reason.clone();
            }
        }
    })
    .await
    .expect("a mute kernel was never ended");
    assert_eq!(reason, "the kernel did not answer its hello within 1s");
    assert!(becomes(Duration::from_secs(10), || spawns(&counter) >= 2).await, "a mute kernel was never replaced");
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the supervisor loop outlived the shutdown").expect("task");
}

/// A kernel that reads nothing fills its pipe, the supervisor sits in its write and the channel fills behind
/// it: every request still ends within the caller's bound, the send included.
#[tokio::test]
async fn a_full_channel_still_answers_within_the_bound() {
    let dir = tempfile::tempdir().unwrap();
    let (project, _counter) = wedged_kernel(dir.path(), "full", true);
    let kernel = Kernel::new(project, dir.path().to_path_buf());
    let big = "x".repeat(64 * 1024);
    let mut calls = Vec::new();
    for n in 0..70 {
        let (kernel, big) = (kernel.clone(), big.clone());
        calls.push(tokio::spawn(async move {
            let began = Instant::now();
            let answered = tokio::time::timeout(Duration::from_secs(15), kernel.request("test.big", json!(big))).await.is_ok();
            (n, answered, began.elapsed())
        }));
    }
    let mut slowest = Duration::ZERO;
    for call in calls {
        let (n, answered, took) = call.await.expect("request task");
        assert!(answered, "request {n} waited 15 s on a kernel that reads nothing");
        slowest = slowest.max(took);
    }
    println!("kernel-full: 70 requests answered; the slowest after {slowest:?}");
    assert!(slowest < Duration::from_secs(11), "the slowest request took {slowest:?}");
    drop(kernel);
}
