use super::*;

/// The one mapping in this module that a wrong answer would hide:
/// `kevent`'s timeout is a POINTER and NULL means "block forever",
/// so the supervisor's own non-blocking tick MUST arrive as a real
/// `timespec { 0, 0 }`. Pure arithmetic, so this is a proof by
/// construction rather than an observation of a running kernel.
#[test]
fn zero_timeout_is_a_real_zero_timespec() {
    assert_eq!(kevent_timeout_parts(Duration::ZERO), (0, 0));
    let ts = kevent_timeout(Duration::ZERO);
    assert_eq!(ts.tv_sec, 0);
    assert_eq!(ts.tv_nsec, 0);
}

#[test]
fn sub_second_and_whole_timeouts_survive_the_split() {
    assert_eq!(
        kevent_timeout_parts(Duration::from_millis(1500)),
        (1, 500_000_000)
    );
    assert_eq!(kevent_timeout_parts(Duration::from_nanos(7)), (0, 7));
}

/// The far end saturates rather than wrapping, mirroring
/// `challenge_unix::poll_pidfd_readable`'s own clamp to `i32::MAX`.
#[test]
fn an_unrepresentable_timeout_saturates() {
    assert_eq!(kevent_timeout_parts(Duration::MAX), (i64::MAX, 0));
}
