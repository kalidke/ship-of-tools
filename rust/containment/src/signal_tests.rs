use super::*;
use crate::leftover::Leftover;
use std::time::Duration;

/// A leftover records the process the test started, from the file the process wrote its own pid to, and holds
/// the test to observing its end: one that is gone is seen gone, one dropped unobserved ends the test with a
/// failure, and one declared alive at the end is left alone. Nothing here signals the process.
#[test]
fn leftover_records_its_child_and_requires_the_end_observation() {
    let dir = tempfile::tempdir().unwrap();
    let start = |name: &str| {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "echo $$ > \"$1\"; exec sleep 3120", "sh"])
            .arg(dir.path().join(name))
            .spawn()
            .unwrap();
        let file = dir.path().join(name);
        let began = std::time::Instant::now();
        while std::fs::read_to_string(&file)
            .map(|t| t.trim().is_empty())
            .unwrap_or(true)
        {
            assert!(
                began.elapsed() < Duration::from_secs(5),
                "the child never wrote its pid"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The child's pid is its own until it is waited for: reading it from its file is what the leftovers do.
        assert!(child.try_wait().unwrap().is_none());
        (child, file)
    };

    // Observed gone: the child is killed and reaped, and the leftover reads the exit from its identity.
    let (mut child, file) = start("seen");
    let leftover = Leftover::of_file(file);
    assert!(leftover.pid().is_some());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(leftover.gone(), "a reaped child was not seen gone");
    drop(leftover);

    // Unobserved: the drop fails the test and signals nothing; the test ends its own child.
    let (mut child, file) = start("unseen");
    let leftover = Leftover::of_file(file);
    assert!(leftover.pid().is_some());
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(leftover)));
    assert!(
        failed.is_err(),
        "a leftover dropped without its end observed did not fail the test"
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "the unobserved leftover's process was signalled"
    );
    child.kill().unwrap();
    child.wait().unwrap();

    // Alive at the end by design: dropped quietly, and not signalled.
    let (mut child, file) = start("kept");
    let leftover = Leftover::of_file(file);
    assert!(leftover.pid().is_some());
    leftover.kept_alive();
    drop(leftover);
    assert!(
        child.try_wait().unwrap().is_none(),
        "the kept-alive leftover's process was signalled"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

/// An owner stuck writing to a child that never reads cannot poll the
/// signal; the fire still takes the child and its grandchild.
#[cfg(unix)]
#[tokio::test]
async fn fire_kills_a_tree_whose_owner_is_blocked() {
    use tokio::io::AsyncWriteExt;
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", "sleep 3104 & echo $!; exec sleep 3104"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut contained = signal.spawn(&mut cmd).expect("spawn");
    assert_eq!(signal.held_groups().len(), 1);
    let mut line = String::new();
    tokio::io::AsyncBufReadExt::read_line(
        &mut tokio::io::BufReader::new(contained.stdout.take().unwrap()),
        &mut line,
    )
    .await
    .unwrap();
    let grandchild = Leftover::of_pid(line.trim().parse().expect("grandchild pid"));
    let mut stdin = contained.stdin.take().unwrap();
    let owner = tokio::spawn(async move {
        let _ = stdin.write_all(&vec![0u8; 1 << 20]).await;
        let _ = contained.wait().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    signal.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), owner)
        .await
        .expect("the blocked owner did not return")
        .expect("owner task");
    assert!(grandchild.gone(), "the grandchild survived the shutdown");
    assert!(signal.held_groups().is_empty());
}

/// A start after the fire creates nothing.
#[cfg(unix)]
#[tokio::test]
async fn a_start_after_the_fire_creates_nothing() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    signal.fire().expect("fire");
    let created = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&created);
    *signal.after_create.lock().unwrap() =
        Some(Box::new(move |_| flag.store(true, Ordering::SeqCst)));
    let mut cmd = tokio::process::Command::new("sleep");
    cmd.arg("3105");
    assert!(
        signal.spawn(&mut cmd).is_err(),
        "a start after the fire was accepted"
    );
    assert!(
        !created.load(Ordering::SeqCst),
        "a start after the fire created a process"
    );
    assert!(signal.held_groups().is_empty());
}

/// The leader's exit takes everything it started with it, before the
/// leader is reaped, even though nothing fired the signal.
#[cfg(unix)]
#[tokio::test]
async fn wait_kills_the_tree_at_the_leaders_exit() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", "sleep 3107 >/dev/null 2>&1 & echo $!"])
        .stdout(std::process::Stdio::piped());
    let mut contained = signal.spawn(&mut cmd).expect("spawn");
    let mut line = String::new();
    tokio::io::AsyncBufReadExt::read_line(
        &mut tokio::io::BufReader::new(contained.stdout.take().unwrap()),
        &mut line,
    )
    .await
    .unwrap();
    let descendant = Leftover::of_pid(line.trim().parse().expect("descendant pid"));
    contained.wait().await.expect("wait");
    assert!(
        descendant.gone(),
        "the leader's descendant survived its exit"
    );
    assert!(
        signal.held_groups().is_empty(),
        "a reaped leader's tree is still held"
    );
    drop(contained);
    assert!(signal.held_groups().is_empty());
}

/// Observation alone neither releases the tree nor reaps the leader: a cancelled observation loses nothing, and
/// after the leader exits the tree is still held with its descendant alive until the checked wait takes it.
#[cfg(unix)]
#[tokio::test]
async fn exit_observation_retains_owner_and_descendant() {
    use tokio::io::AsyncWriteExt;
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", "sleep 3211 & echo $!; read x"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut contained = signal.spawn(&mut cmd).expect("spawn");
    let mut stdout = tokio::io::BufReader::new(contained.stdout.take().unwrap());
    let mut line = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut stdout, &mut line).await.unwrap();
    let descendant = Leftover::of_pid(line.trim().parse().expect("descendant pid"));

    let live = tokio::time::timeout(Duration::from_millis(200), contained.wait_until_exited()).await;
    assert!(live.is_err(), "a live leader must not be reported exited");
    assert_eq!(signal.held_groups().len(), 1);

    contained.stdin.take().unwrap().write_all(b"\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), contained.wait_until_exited())
        .await
        .expect("the exit must be observed")
        .expect("observe");
    assert_eq!(signal.held_groups().len(), 1, "observation released the tree");
    // SAFETY: signal 0 only probes the descendant this test started.
    assert_eq!(unsafe { libc::kill(descendant.pid().expect("pid"), 0) }, 0, "observation took the descendant");

    contained.wait().await.expect("the checked wait");
    assert!(descendant.gone(), "the checked wait left the descendant");
    assert!(signal.held_groups().is_empty());
    drop(contained);
    assert_eq!(signal.live(), 0);
}

/// A blocking caller sees the exit unreaped: the leader's number is still
/// its own and the tree is still held, until the caller waits; only then
/// is the tree killed and the leader reaped.
#[cfg(unix)]
fn one_shot_takes_its_tree_after_it_exits() {
    use std::io::BufRead;
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "sleep 3107 >/dev/null 2>&1 & echo $!"])
        .stdout(std::process::Stdio::piped());
    let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
    let pgid = c.id() as i32;
    let mut line = String::new();
    std::io::BufReader::new(c.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let descendant = Leftover::of_pid(line.trim().parse().expect("descendant pid"));
    assert!(c.exited(true).expect("exited"), "the child had not exited");
    assert!(
        signal.held_groups().contains(&pgid),
        "the tree was released before its owner let go"
    );
    // SAFETY: signal 0 only probes the pid.
    assert_eq!(
        unsafe { libc::kill(pgid, 0) },
        0,
        "the exit was seen by reaping it"
    );
    c.wait().expect("wait");
    assert!(
        signal.held_groups().is_empty(),
        "a reaped leader's tree is still held"
    );
    assert!(descendant.gone(), "the one-shot's descendant survived");
}

/// A one-shot's output is read to its end, and the tree dies after the
/// leader's exit: a descendant the exit leaves behind does not survive,
/// and nothing stays registered or counted.
#[cfg(unix)]
#[test]
fn output_takes_the_tree_after_the_exit() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "sleep 3111 >/dev/null 2>&1 & echo $!"]);
    let out = signal.output(&mut cmd, std::time::Duration::from_secs(60)).expect("output");
    assert!(out.status.success(), "{}: {}", out.status, String::from_utf8_lossy(&out.stderr));
    let descendant = Leftover::of_pid(
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("descendant pid"),
    );
    assert!(descendant.gone(), "the one-shot's descendant survived");
    assert!(
        signal.held_groups().is_empty(),
        "a reaped leader's tree is still held"
    );
    assert!(signal.held_groups().is_empty());
}

#[cfg(unix)]
#[test]
fn a_one_shot_takes_its_tree_after_it_exits() {
    one_shot_takes_its_tree_after_it_exits();
}

/// A parent that started the daemon with `SIGCHLD` ignored must not make
/// the kernel reap contained leaders; the startup reset undoes it. The
/// disposition is process-wide, so the scenario runs in a re-executed copy
/// of this test binary and no test in this process touches `SIGCHLD`.
#[cfg(unix)]
#[test]
fn an_ignored_sigchld_is_reset_at_startup() {
    let name = "signal::tests::ignored_sigchld_scenario";
    let ran = sot_platform::test_wait::run_within(
        std::process::Command::new(std::env::current_exe().expect("current_exe"))
            .args(["--exact", name, "--test-threads=1", "--nocapture"])
            .env("SOT_TEST_SIGCHLD_CHILD", "1"),
        sot_platform::test_isolated::ISOLATION_TIMEOUT,
    );
    let text = format!("{}{}", ran.stdout, ran.stderr);
    assert!(ran.status.success(), "the child run failed:\n{ran}");
    assert!(
        text.contains(&format!("test {name} ... ok")),
        "the scenario did not run:\n{text}"
    );
}

/// The re-executed half of `an_ignored_sigchld_is_reset_at_startup`; it
/// does nothing in an ordinary run.
#[cfg(unix)]
#[test]
fn ignored_sigchld_scenario() {
    if std::env::var_os("SOT_TEST_SIGCHLD_CHILD").is_none() {
        return;
    }
    // SAFETY: a plain disposition change in a process of its own.
    unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
    reset_child_signal();
    one_shot_takes_its_tree_after_it_exits();
}

/// A parent that started the daemon with `SIGCHLD` blocked would hide every exit from `Contained::wait`, which
/// learns of one from the signal; the startup reset unblocks it. The mask is the starting thread's and survives
/// exec, so the scenario runs in a re-executed copy of this test binary whose parent blocked it.
#[cfg(unix)]
#[test]
fn a_blocked_sigchld_is_unblocked_at_startup() {
    use std::os::unix::process::CommandExt;
    let name = "signal::tests::blocked_sigchld_scenario";
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("current_exe"));
    cmd.args(["--exact", name, "--test-threads=1", "--nocapture"])
        .env("SOT_TEST_SIGCHLD_BLOCKED", "1");
    // SAFETY: only sigprocmask on a set built in place, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGCHLD);
            if libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let ran = sot_platform::test_wait::run_within(&mut cmd, sot_platform::test_isolated::ISOLATION_TIMEOUT);
    let text = format!("{}{}", ran.stdout, ran.stderr);
    assert!(ran.status.success(), "the child run failed:\n{ran}");
    assert!(
        text.contains(&format!("test {name} ... ok")),
        "the scenario did not run:\n{text}"
    );
}

/// The re-executed half of `a_blocked_sigchld_is_unblocked_at_startup`; it does nothing in an ordinary run.
#[cfg(unix)]
#[test]
fn blocked_sigchld_scenario() {
    if std::env::var_os("SOT_TEST_SIGCHLD_BLOCKED").is_none() {
        return;
    }
    reset_child_signal();
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("1");
        let mut contained = signal.spawn(&mut cmd).expect("spawn");
        let status = tokio::time::timeout(Duration::from_secs(10), contained.wait())
            .await
            .expect("the wait never saw the leader's exit")
            .expect("wait");
        assert!(status.success(), "the leader ended: {status}");
    });
}

/// Once a `ContainedStd` has reaped its child it answers from the reap: the freed pid is never probed again.
#[cfg(unix)]
#[test]
fn a_reaped_child_answers_from_its_reap() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("3112");
    let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
    let status = c.kill().expect("kill");
    assert!(
        c.exited(false).expect("exited after the reap"),
        "a reaped child is not seen as exited"
    );
    assert_eq!(c.wait().expect("wait after the reap"), status);
    assert_eq!(c.kill().expect("kill after the reap"), status);
}

/// A leader that moved to another process group is outside its tree's group kill, so the fire must take it by
/// pid as well. A killed leader stays a zombie until its owner reaps it, so "gone" is seen as an exit, unreaped.
#[cfg(unix)]
#[test]
fn a_leader_that_left_its_group_dies_at_the_fire() {
    use std::os::unix::process::CommandExt;
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    // SAFETY: a plain read of this process's own group.
    let theirs = unsafe { libc::getpgrp() };
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("3114")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: only setpgid, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setpgid(0, theirs) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
    signal.fire().expect("fire");
    let began = std::time::Instant::now();
    let mut exited = false;
    while !exited && began.elapsed() < Duration::from_secs(3) {
        exited = c.exited(false).expect("exited");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(exited, "a leader that left its group survived the fire");
    drop(c);
}

/// A bounded wait returns the status of a child that exits in time.
#[cfg(unix)]
#[test]
fn wait_within_returns_a_prompt_exit() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "exit 3"]);
    let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
    let status = c
        .wait_within(Duration::from_secs(10))
        .expect("wait_within")
        .expect("the child exited in time");
    assert_eq!(status.code(), Some(3));
}

/// A child past its bound is ended with its tree, reaped, and counted out.
#[cfg(unix)]
#[test]
fn wait_within_ends_a_child_past_its_bound() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("3113");
    let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
    assert!(
        c.wait_within(Duration::from_millis(200))
            .expect("wait_within")
            .is_none(),
        "a child past its bound was waited for"
    );
    assert!(signal.held_groups().is_empty(), "the tree is still held");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&c.wait().expect("the reaped status")),
        Some(libc::SIGKILL),
        "the child past its bound was not killed"
    );
    drop(c);
    assert!(signal.held_groups().is_empty());
}
