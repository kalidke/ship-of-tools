use super::{parse_start_ticks, ChallengedProcess};
use std::io;
use std::time::Duration;

/// A socketpair's peer is this very process, so `SO_PEERCRED` and
/// the `/proc` stamp must agree with `self_identity`.
#[test]
fn self_identity_matches_own_peer_identity() {
    use std::os::fd::AsRawFd;
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    let cred = super::peer_credentials(a.as_raw_fd()).unwrap();
    let me = crate::identity::challenge::self_identity().unwrap();
    assert_eq!(cred.pid, me.pid);
    assert_eq!(cred.pid, std::process::id());
    assert_eq!(crate::identity::challenge::process_created(cred.pid).unwrap(), me.created);
    assert_eq!(me.boot, crate::identity::challenge::boot_identity().unwrap());
}

/// A literal `/proc/pid/stat`-shaped line whose `comm` field (the
/// second, parenthesized field) contains BOTH a space and a nested
/// close-paren -- the exact shape that breaks a naive
/// first-`(`-to-first-`)` parse. Field 22 ("starttime") is the 20th
/// token after the LAST `)`.
#[test]
fn parses_starttime_past_a_comm_with_spaces_and_parens() {
    // pid=1234 comm="weird (proc) name" state=S ppid..majflt (fields
    // 4-9, six tokens) utime cutime cstime priority nice num_threads
    // itrealvalue (fields 14-20, seven tokens) starttime=987654321
    // (field 22) ...
    let line = "1234 (weird (proc) name) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 0 20 0 1 987654321 0 0";
    assert_eq!(parse_start_ticks(line), Some(987654321));
}

#[test]
fn rejects_a_line_with_no_closing_paren() {
    assert_eq!(parse_start_ticks("no parens here at all"), None);
}

#[test]
fn rejects_a_line_with_too_few_fields_after_comm() {
    assert_eq!(parse_start_ticks("1234 (comm) S 1 1"), None);
}

/// Single-owner reaping (review round): `ChallengedProcess::reap` is
/// the OWNER's explicit call, made after `wait` has already observed
/// the exit. Spawns `/bin/sh -c "exit 0"` directly (never through
/// `std::process::Child::wait`/`try_wait`, which would reap it
/// themselves and defeat the very thing this test proves) so the
/// child is still an unreaped zombie when the freshly built
/// `ChallengedProcess` observes its exit via `wait`; `reap()` then
/// frees the pid — a further `waitpid(pid, WNOHANG)` must now report
/// `ECHILD` (already reaped by THIS call), never `0` (still a live
/// zombie) or a real block.
#[test]
fn reap_after_wait_frees_the_pid() {
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        // Never inherit this test binary's own stdout/stderr: an
        // inherited fd held open by the CHILD keeps any pipe reading
        // this process's own output from ever seeing EOF, regardless
        // of how promptly the child itself is reaped.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let pidfd = super::pidfd_open(pid).expect("pidfd_open");
    let start = super::process_start_ticks(pid).expect("process_start_ticks");
    let proof = ChallengedProcess::from_pinned_for_test(pidfd, pid, start);
    assert!(
        proof.wait(Duration::from_secs(5)).expect("wait"),
        "expected the child to exit within 5s"
    );
    proof.reap();
    let mut status: libc::c_int = 0;
    let rc = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    let err = io::Error::last_os_error();
    assert_eq!(rc, -1, "expected waitpid to report the child already reaped (rc={rc})");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ECHILD),
        "expected ECHILD (already reaped by reap()), got {err:?}"
    );
}

/// The other half of single-owner reaping: `Drop` never reaps, full
/// stop — not just "never a still-live process" (the old test's own
/// scope) but not even an already-exited one, which is exactly the
/// scenario a daemon holding BOTH a `tokio::process::Child` and a
/// `ChallengedProcess` for the same pid depends on (only `Child::wait`
/// may ever reap that pid). Spawns `/bin/sh -c "exit 0"` directly
/// (same reasoning as the sibling test above), observes its exit via
/// `wait`, then DROPS the handle: a subsequent `waitpid(pid,
/// WNOHANG)` from THIS test must itself successfully reap the still-
/// zombie child (`rc == pid`), proving `Drop` left it untouched.
#[test]
fn drop_never_reaps() {
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let pidfd = super::pidfd_open(pid).expect("pidfd_open");
    let start = super::process_start_ticks(pid).expect("process_start_ticks");
    let proof = ChallengedProcess::from_pinned_for_test(pidfd, pid, start);
    assert!(
        proof.wait(Duration::from_secs(5)).expect("wait"),
        "expected the child to exit within 5s"
    );
    drop(proof);
    // The test itself now owns the reap -- proving Drop left the
    // zombie fully intact for whoever the real owner is.
    let mut status: libc::c_int = 0;
    let rc = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    assert_eq!(
        rc, pid as libc::pid_t,
        "expected this test's own waitpid to reap the child (Drop must not have already done so)"
    );
}
