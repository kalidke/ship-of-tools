//! The producer dies with its capsule: PDEATHSIG observed through /proc.

use super::*;

/// The direct children of `pid`, via `/proc/<pid>/task/<pid>/children`
/// (Linux 3.5+) — the capsule's fork of its producer happens on its own
/// main thread, before any of its own extra threads exist (the reader
/// thread starts only AFTER a successful spawn — see `capsule/`'s own
/// `run`), so at fork time this thread's own tid still equals the
/// process's own pid, making this exactly the producer's own pid with no
/// name-matching needed (a shared CI runner may have unrelated `sleep`
/// processes of its own).
fn read_direct_children(pid: u32) -> Vec<u32> {
    let path = format!("/proc/{pid}/task/{pid}/children");
    std::fs::read_to_string(path).unwrap_or_default().split_whitespace().filter_map(|s| s.parse().ok()).collect()
}

/// `/proc/<pid>/comm`'s own content, trimmed — used below to confirm the
/// producer has already exec'd into `sleep` (not merely into the `/bin/sh`
/// wrapper that ignores SIGHUP on its behalf).
fn proc_comm(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
}

/// `/proc/<pid>/stat` field 6 (session id) — used below to confirm
/// `setsid()` has already run (it always runs before ANY exec in this
/// producer's own `pre_exec`, so this is really always true by the time
/// the process exists at all, but checking it costs nothing and matches
/// the review round's own request for a robust readiness probe).
fn proc_session_id(pid: u32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    // fields after the comm's closing paren, 0-indexed: 0=state(3),
    // 1=ppid(4), 2=pgrp(5), 3=session(6).
    stat[close + 1..].split_whitespace().nth(3)?.parse().ok()
}

/// `/proc/<pid>/stat` field 3 (state) — review round 2 (R7): a zombie
/// still answers `kill(pid, 0)` as "exists" (its pid stays valid until
/// reaped), so that alone cannot prove PDEATHSIG actually did anything.
/// The real proof is state `Z`, or the `/proc` entry being gone entirely
/// (this producer reparents to a subreaper once the capsule dies and is
/// typically reaped quickly) — reuses `proc_session_id`'s own parser
/// shape for the state field instead.
fn proc_state(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    stat[close + 1..].split_whitespace().next().map(str::to_string)
}

/// ADR 0043 decision 14: `PR_SET_PDEATHSIG(SIGKILL)` must fire on the
/// death of the SPAWNING THREAD by ANY means, including a hard `SIGKILL`
/// of the whole capsule process — not merely an orderly exit. Spawns a
/// real `sot-capsule run` binary; polls for the producer's own child pid
/// to appear (see `read_direct_children`'s own doc — `Transport::bind`
/// actually runs BEFORE `Producer::spawn` in `capsule::run`'s own
/// ordering, the opposite of what an earlier version of this test assumed
/// from the socket's own appearance alone), then waits for it to become a
/// READY `sleep` (post-exec, `comm == "sleep"`, session id == its own
/// pid) before ever killing the capsule.
///
/// Review round (F7): the producer's argv is `sh -c 'trap "" HUP; exec
/// sleep 600'`, not a bare `sleep 600` — a bare `sleep` does NOT ignore
/// `SIGHUP`, and the capsule's own master fd closing when SIGKILLed
/// triggers a real tty hangup, which the kernel delivers as `SIGHUP` to
/// the pty's foreground process group regardless of whether `PDEATHSIG`
/// even fired at all. With `SIGHUP` ignored (`SIG_IGN` survives `exec`,
/// unlike a caught handler), the ONLY thing that can end the producer
/// once the capsule dies is `PR_SET_PDEATHSIG` itself — proving the
/// property this test exists for, not a confound. The final check is
/// "the producer pid is gone within 5s", not a captured termination
/// signal: this test is not the producer's own reaper (the capsule was),
/// so once the capsule dies the producer reparents to a subreaper, which
/// typically reaps it quickly — there is no reliable window in which an
/// outside, non-parent observer could read back "died by signal 9" from
/// `/proc` before that reap removes the entry entirely.
#[test]
fn pdeathsig_kills_the_producer_when_the_capsule_dies_hard() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let voyage_id = fresh_voyage_id();
    let voyage_root = dir.path().join(&voyage_id);

    let capsule_exe = env!("CARGO_BIN_EXE_sot-capsule");
    let mut command = std::process::Command::new(capsule_exe);
    command
        .arg("run")
        .arg(&voyage_root)
        .arg(&voyage_id)
        .arg("--assume-no-rollback-target")
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg("trap '' HUP; exec sleep 600")
        .env("SOT_RUNTIME_DIR", _runtime.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = capsule_guard::CapsuleGuard::spawn(&mut command);
    let capsule_pid = child.id();

    // Poll directly for the producer's own child pid to appear -- the
    // actual precondition this test needs (not merely "the capsule got
    // far enough to bind its socket", which happens EARLIER and proves
    // nothing about whether `Producer::spawn` has run yet).
    let deadline = Instant::now() + Duration::from_secs(10);
    let producer_pid = loop {
        if let Some(pid) = read_direct_children(capsule_pid).into_iter().next() {
            break pid;
        }
        if Instant::now() >= deadline {
            panic!("the capsule never spawned a producer child within 10s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    // Wait for the producer to be a READY, post-exec `sleep` (SIGHUP
    // already ignored) before ever killing the capsule -- see this
    // test's own doc for why both checks matter.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if proc_comm(producer_pid).as_deref() == Some("sleep")
            && proc_session_id(producer_pid) == Some(producer_pid as i32)
        {
            break;
        }
        if Instant::now() >= deadline {
            panic!("the producer never became a ready, post-exec `sleep` session leader within 10s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // Hard-kill the capsule itself -- SIGKILL, no graceful EndRun at all.
    child.child_mut().kill().expect("SIGKILL the capsule");
    let _ = child.child_mut().wait();

    // `PR_SET_PDEATHSIG(SIGKILL)` fires on the death of the SPAWNING
    // THREAD (the capsule's own main thread, just killed above) -- the
    // producer itself must be DEAD within a bounded wait, with SIGHUP
    // ignored and no supervisor or teardown code involved at all.
    // Review round 2 (R7): "dead" means state `Z` or the `/proc` entry
    // gone entirely -- NOT `kill(pid, 0) == ESRCH` alone, which a zombie
    // still answers as "exists" (its pid stays valid until reaped), so
    // that check alone could not distinguish "PDEATHSIG fired, awaiting
    // reap" from "PDEATHSIG never fired at all".
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match proc_state(producer_pid) {
            Some(s) if s == "Z" => break,
            None => break, // /proc entry gone -- also dead
            Some(_) => {}  // still alive in some other state
        }
        assert!(
            Instant::now() < deadline,
            "the producer (SIGHUP ignored) was still alive 5s after the capsule was SIGKILLed -- \
             PDEATHSIG did not fire"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
