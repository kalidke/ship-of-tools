//! Fact 2: closing the pty master hangs up and reaps the child.

use super::*;

// ---------------------------------------------------------------------------
// Fact 2: pty hangup
// ---------------------------------------------------------------------------

/// A raw `openpty` + `setsid`/`TIOCSCTTY`/`dup2` spawn, NOT
/// `PtyProducer::spawn`. Deliberate: `PtyProducer` holds the slave for the
/// whole run and its `Drop` kills the child's process group, which is
/// exactly the behaviour under test -- driving it through that type would
/// measure the producer's own killing, not the kernel's. The shape below
/// is `capsule/producer/pty/`'s own `pre_exec` sequence, minus the Linux-only
/// PDEATHSIG arming that is the reason this fact matters at all.
#[test]
fn closing_the_pty_master_hangs_up_and_reaps_the_child() {
    pty_hangup_reaps_the_child(SlaveHeld::DroppedRightAfterSpawn);
}

/// Fact 2b — the SHIPPED configuration. Fact 2 above drops the parent's
/// slave immediately after the spawn, so the child's own stdio is the only
/// reference left and closing the master is an ordinary last-close hangup.
/// The capsule does the OPPOSITE: ADR 0043 decision 12 makes it HOLD a
/// slave for the whole run (that is what keeps a mid-run EIO from being
/// mistaken for the child's death), and `close_output_side` drops it only
/// at teardown. So fact 2 pins its behaviour in a configuration the
/// product never runs, and the pty module doc's parent-death argument —
/// which is why the macOS lane ships no lease — rests on this variant
/// instead.
///
/// The expectation is that it behaves identically, because the hangup is a
/// carrier drop on the MASTER's last close and is not gated on how many
/// openers the slave side has. A SURVIVING child here is not a flaky test:
/// it means a dead capsule can leave an agent running forever on a user's
/// Mac, which is exactly the failure the lease deletion promised could not
/// happen. That is a ruling to reopen, not an assertion to relax.
#[test]
fn closing_the_pty_master_hangs_up_and_reaps_the_child_with_the_slave_held() {
    pty_hangup_reaps_the_child(SlaveHeld::AcrossTheMasterClose);
}

/// WHEN the parent lets go of its own slave copy — the one difference
/// between fact 2 and fact 2b, and the only thing this body branches on.
#[derive(Clone, Copy, PartialEq)]
enum SlaveHeld {
    DroppedRightAfterSpawn,
    AcrossTheMasterClose,
}

impl SlaveHeld {
    fn label(self) -> &'static str {
        match self {
            SlaveHeld::DroppedRightAfterSpawn => "fact 2",
            SlaveHeld::AcrossTheMasterClose => "fact 2b",
        }
    }
}

#[allow(clippy::too_many_lines, reason = "one test scenario: a pty hangup reaps the child")]
fn pty_hangup_reaps_the_child(slave_held: SlaveHeld) {
    let label = slave_held.label();
    let mut master_fd: libc::c_int = -1;
    let mut slave_fd: libc::c_int = -1;
    // SAFETY: both out-params are live locals; NULL is documented-valid
    // for `name`/`termp`/`winp` (the pty then takes system defaults).
    let rc = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        rc,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: both fds were just returned by `openpty` and are owned here.
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };

    // CLOEXEC on BOTH before the fork, exactly as `capsule/producer/pty/` does
    // it -- and here it is load-bearing for the measurement, not only for
    // hygiene: if the child inherited a copy of the MASTER, closing the
    // parent's master would not hang the pty up at all and this test would
    // report a false "the child survived". The `dup2`s below re-open the
    // slave on 0..=2 (which clears the flag on those), so the child keeps
    // exactly the slave and nothing else.
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: both fds are live and owned by this function.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            assert!(flags >= 0, "F_GETFD failed: {}", std::io::Error::last_os_error());
            assert!(
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) >= 0,
                "F_SETFD FD_CLOEXEC failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    let slave_raw = slave.as_raw_fd();
    let mut cmd = Command::new("/bin/sh");
    // `printf READY` announces that the child has already run `setsid` +
    // `TIOCSCTTY` (both happen in `pre_exec`, before this shell exists),
    // which closes the race where the master is closed BEFORE the child
    // owns the pty -- a race that would also look like "it survived".
    // `exec sleep 60` then makes the long-lived process the one holding
    // the controlling terminal, with no shell in between.
    cmd.arg("-c").arg("printf READY; exec sleep 60");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: this closure runs between fork and exec -- only
    // async-signal-safe calls, per `pre_exec`'s contract. `setsid`,
    // `ioctl`, `dup2` and `close` all are (same set `capsule/producer/pty/`'s
    // own `pre_exec` uses).
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for fd in 0..3 {
                if libc::dup2(slave_raw, fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if slave_raw > 2 {
                libc::close(slave_raw);
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().expect("spawn /bin/sh on the pty slave");
    let child_pid = child.id();
    // Fact 2: the parent's own slave copy goes NOW, so from here the
    // child's stdio is the only reference to the slave side and closing
    // the master is a last-close hangup and nothing else. Fact 2b: it
    // stays open across the close, which is what the shipped capsule does
    // (decision 12) and what this variant exists to measure.
    let held_slave = match slave_held {
        SlaveHeld::DroppedRightAfterSpawn => {
            drop(slave);
            None
        }
        SlaveHeld::AcrossTheMasterClose => Some(slave),
    };

    // Non-blocking master + a polled read: a bounded wait for READY that
    // cannot wedge the macOS job.
    // SAFETY: `master` is live and owned here.
    unsafe {
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        assert!(flags >= 0, "F_GETFL failed: {}", std::io::Error::last_os_error());
        assert!(
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0,
            "F_SETFL O_NONBLOCK failed: {}",
            std::io::Error::last_os_error()
        );
    }
    let mut master_file = std::fs::File::from(master);
    let mut seen: Vec<u8> = Vec::new();
    let mut ready_err = String::new();
    let ready_deadline = Instant::now() + PTY_READY_TIMEOUT;
    while !seen.windows(5).any(|w| w == b"READY") {
        let mut buf = [0u8; 256];
        match master_file.read(&mut buf) {
            Ok(0) => {
                ready_err = "the pty master reported EOF".into();
                break;
            }
            Ok(n) => seen.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= ready_deadline {
                    ready_err = format!("nothing arrived within {PTY_READY_TIMEOUT:?}");
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                ready_err = format!("reading the pty master failed: {e}");
                break;
            }
        }
    }
    if !ready_err.is_empty() {
        let status = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
        panic!(
            "the pty child never announced READY ({ready_err}); it never took the pty as \
             its controlling terminal, so this run says nothing about hangup. child pid \
             {child_pid}, {status}, bytes seen on the master: {:?}",
            String::from_utf8_lossy(&seen)
        );
    }

    // THE measurement: close the master, then poll for the child's death.
    let closed_at = Instant::now();
    drop(master_file);
    let deadline = closed_at + PTY_REAP_TIMEOUT;
    let mut outcome = None;
    loop {
        match child.try_wait().expect("try_wait on the pty child") {
            Some(status) => {
                outcome = Some((status, closed_at.elapsed()));
                break;
            }
            None => {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }

    let Some((status, took)) = outcome else {
        let status = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
        drop(held_slave);
        panic!(
            "{label}: the child (pid {child_pid}) SURVIVED closing the pty master for \
             {PTY_REAP_TIMEOUT:?} ({status} after the test killed it). This is a DESIGN \
             INPUT, not a defect in this test: macOS has no `PR_SET_PDEATHSIG`, so if pty \
             hangup does not reap a producer either, the pipe lease becomes the only \
             mechanism that reaps an orphaned producer and must be treated as \
             load-bearing rather than as belt-and-braces. Do not relax this assertion. \
             For fact 2b specifically (the parent held a slave across the close, which is the \
             SHIPPED shape) this reopens the deleted macOS parent-death lease: read decision \
             12's amendment before touching anything."
        );
    };
    drop(held_slave);

    fact(&format!(
        "{label}: closing the pty master ended the child (pid {child_pid}) in {}ms -- \
         signal={:?} code={:?}",
        took.as_millis(),
        status.signal(),
        status.code()
    ));
    assert!(
        status.signal().is_some(),
        "{label}: the child (pid {child_pid}) ended {took:?} after the master closed, but by plain \
         exit (code={:?}), not by a signal -- `sleep 60` cannot exit on its own that fast, \
         so this run did not observe a hangup kill and proves nothing either way. \
         Investigate the spawn, do not relax the assertion.",
        status.code()
    );
}
