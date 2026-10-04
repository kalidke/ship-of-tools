//! Fact 7: what revoking the pty master costs the record of the final output.

use super::*;

// ---------------------------------------------------------------------------
// Fact 7: what the revoke costs the record
// ---------------------------------------------------------------------------

/// Bytes the fact-7 child writes before it exits. Deliberately modest: a
/// tty's output queue is a kernel clist of a few KiB, and a child whose
/// write does not FIT that queue blocks until someone drains it, which
/// would measure the test's own reading rather than the kernel's revoke.
/// The test reports that case as its fact instead of asserting anything.
const REVOKE_PROBE_BYTES: usize = 1024;
/// How long fact 7 waits for its child to write its bytes and exit WITHOUT
/// reading the master -- the whole point being to leave those bytes sitting
/// in the tty queue at the instant the session leader exits.
const REVOKE_EXIT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the post-exit drain is given before its silence is believed.
const REVOKE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Fact 7 — HOW MUCH of a producer's final output does the revoke throw
/// away?
///
/// ADR 0043 decision 12, as amended: on macOS a session leader's exit
/// revokes every descriptor on its controlling terminal, and the revoke
/// flushes the tty's queues. So bytes the producer wrote but the capsule's
/// reader had not yet drained at that instant can be discarded — an
/// agent's last line before exiting may simply be missing from the sealed
/// record. The ADR names that loss; this test measures its size on a real
/// Mac, in the SHIPPED configuration (the parent holds a slave for the
/// whole run, per decision 12).
///
/// It asserts NO number. The quantity is a kernel property that a future
/// XNU may change, and the honest thing for a record-keeping system is to
/// print what it is rather than to pin a figure this project does not
/// control. It also reports HOW the master ended — `Ok(0)` or `EIO` —
/// because `capsule/`'s rule admits either (Darwin may report a revoked
/// master as either, and both reach the same arm).
#[test]
fn a_revoked_pty_master_reports_how_much_of_the_final_output_survived() {
    let mut master_fd: libc::c_int = -1;
    let mut slave_fd: libc::c_int = -1;
    // SAFETY: both out-params are live locals; NULL is documented-valid
    // for `name`/`termp`/`winp`.
    let rc = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    // SAFETY: both fds were just returned by `openpty` and are owned here.
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
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
    // `%<N>d` pads to exactly N bytes with no newline, so nothing here
    // depends on the tty's own ONLCR translation; `exec /usr/bin/true`
    // then makes the leader's exit IMMEDIATE and unconditional, with no
    // shell left in between to slow the revoke down.
    cmd.arg("-c")
        .arg(format!("printf '%{REVOKE_PROBE_BYTES}d' 0; exec /usr/bin/true"));
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: fork-to-exec closure -- `setsid`, `ioctl`, `dup2` and
    // `close` only, the same async-signal-safe set `capsule/producer/pty/`'s own
    // `pre_exec` uses.
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
    // The parent KEEPS its slave: that is the shipped shape (decision 12),
    // and it is what makes this a measurement of the revoke rather than of
    // an ordinary last-close hangup.

    // Nothing is read while the child runs -- the bytes must still be in
    // the tty queue when the leader exits, which is the exposure the ADR
    // names.
    let exit_deadline = Instant::now() + REVOKE_EXIT_TIMEOUT;
    let mut exited = None;
    while Instant::now() < exit_deadline {
        match child.try_wait().expect("try_wait on the pty child") {
            Some(status) => {
                exited = Some(status);
                break;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }

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
    let mut got = 0usize;
    let mut ended = String::from("still readable when the drain timed out");
    let drain_deadline = Instant::now() + REVOKE_DRAIN_TIMEOUT;
    loop {
        let mut buf = [0u8; 256];
        match master_file.read(&mut buf) {
            Ok(0) => {
                ended = "Ok(0) (end of stream)".into();
                break;
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= drain_deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                ended = format!("Err({e}) errno={:?}", e.raw_os_error());
                break;
            }
        }
    }

    let Some(status) = exited else {
        // The child never exited while nothing drained the master: its
        // write did not fit the tty's output queue and blocked. That is
        // itself the fact -- the queue is smaller than the probe -- and it
        // means this run measured nothing about the revoke.
        let reaped = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
        drop(slave);
        fact(&format!(
            "fact 7 INCONCLUSIVE: the child (pid {child_pid}) had not exited \
             {REVOKE_EXIT_TIMEOUT:?} after being asked to write {REVOKE_PROBE_BYTES} bytes \
             with nothing draining the master -- the tty output queue is smaller than the \
             probe, so its write blocked. {got} bytes arrived once the drain started; the \
             master ended with {ended}; {reaped}. Lower REVOKE_PROBE_BYTES to measure the \
             revoke itself."
        ));
        return;
    };
    drop(slave);
    fact(&format!(
        "fact 7: of {REVOKE_PROBE_BYTES} bytes written by the pty child (pid {child_pid}) \
         immediately before it exited ({status}), {got} were still readable on the master \
         AFTER the session leader's exit revoked the terminal -- {} lost. The master ended \
         with {ended}. This is the size of the ADR 0043 decision-12 amendment's named loss \
         (a producer's final undrained output) on this kernel; nothing here asserts a \
         number, because the number is the kernel's to choose.",
        REVOKE_PROBE_BYTES.saturating_sub(got)
    ));
}
