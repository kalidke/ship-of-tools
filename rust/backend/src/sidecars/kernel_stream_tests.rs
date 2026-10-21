use super::deadline_tests::{hello_answer, running, spawns, stub_kernel};
use super::*;

/// Runs a stand-in kernel that answers its hello and then runs `body`, sends it two `file.parse` requests (ids 2
/// and 3), and returns their answers and how many times it was spawned.
async fn two_answers(body: &str) -> (Vec<std::result::Result<Value, String>>, usize) {
    let dir = tempfile::tempdir().unwrap();
    let (project, counter) = stub_kernel(dir.path(), "bytes", &format!("{}{body}exec sleep 600\n", hello_answer()));
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let (status, mut status_rx) = watch::channel(Status::Starting);
    let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
    let tx = running(&mut status_rx).await;
    let mut answers = Vec::new();
    for _ in 0..2 {
        let answer = submit_and_await(&tx, "file.parse", json!({}), KERNEL_REQUEST_TIMEOUT).await;
        answers.push(answer.map_err(|e| format!("{e:#}")));
    }
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the supervisor loop outlived the shutdown").expect("task");
    (answers, spawns(&counter))
}

/// A reply may hold any byte (JSON3 writes a string's bytes as they are, and a parse error quotes its source): it
/// arrives with U+FFFD for the byte, and the same generation answers the next request.
#[tokio::test]
async fn a_kernel_reply_holding_an_invalid_byte_keeps_its_generation() {
    let (answers, spawns) = two_answers(
        "read l\nprintf '{\"id\":2,\"payload\":{\"error\":\"a\\377b\"}}\\n'\nread l\necho '{\"id\":3,\"payload\":{\"ok\":true}}'\n",
    )
    .await;
    assert_eq!((answers, spawns), (vec![Ok(json!({"error": "a\u{fffd}b"})), Ok(json!({"ok": true}))], 1));
}

/// A kernel that fails its start leaves its own words in the log at warn: the stand-in writes Julia's error to
/// stderr and exits before its hello.
#[cfg(unix)]
#[tokio::test]
async fn a_kernel_that_fails_its_start_logs_its_stderr_at_warn() {
    let log = sot_log::test_log::capture();
    let dir = tempfile::tempdir().unwrap();
    let (project, _) = stub_kernel(dir.path(), "kp", "echo 'ERROR: LoadError: (kernel-boot-red)' >&2\nexit 1\n");
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let (status, _keep) = watch::channel(Status::Starting);
    let task = tokio::spawn(supervisor_loop(project, dir.path().to_path_buf(), status, sig, KernelBounds::PRODUCT));
    crate::sidecars::contract_tests::within(Duration::from_secs(10), "the kernel's stderr is in the log at warn", || {
        crate::sidecars::warned(&log.text(), "kernel-boot-red")
    })
    .await;
    sig.fire().expect("fire");
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

/// A stderr line may hold any byte too: the drain reads on past it, so the kernel's later stderr (more than a pipe
/// holds) neither waits for a reader nor fails its write.
#[tokio::test]
async fn a_kernel_stderr_line_holding_an_invalid_byte_keeps_its_generation() {
    let (answers, spawns) = two_answers(
        "read l\nprintf 'warn a\\377b\\n' >&2\ni=0\nwhile [ $i -lt 2048 ]; do echo 'more of the stderr, about sixty-four bytes on every line of it.' >&2 || exit 3; i=$((i + 1)); done\necho '{\"id\":2,\"payload\":{\"ok\":true}}'\nread l\necho '{\"id\":3,\"payload\":{\"ok\":true}}'\n",
    )
    .await;
    assert_eq!((answers, spawns), (vec![Ok(json!({"ok": true})), Ok(json!({"ok": true}))], 1));
}
