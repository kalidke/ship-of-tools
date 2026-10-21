use super::*;

/// Asserted with the TIMESTAMP FIELD REMOVED, which is the whole point.
///
/// The obvious test — draw two names, assert they differ — is worthless
/// here: on a Linux runner the nanosecond clock separates them for free, so
/// it passes whether or not the counter exists and would have stayed green
/// through the exact defect it claims to cover. Only the clock-independent
/// comparison decides anything.
#[test]
fn suffix_is_unique_without_the_clocks_help() {
    let drawn: Vec<String> = (0..16).map(|_| suffix()).collect();
    let mut without_clock: Vec<String> = drawn
        .iter()
        .map(|s| {
            let mut parts: Vec<&str> = s.split('-').collect();
            // Exactly three: pid, nanos, seq. Pinned rather than "at least
            // two", because if the counter were deleted the shape would be
            // two fields and this helper would strip the PID instead of the
            // clock — leaving the names distinct by timestamp and passing
            // through the regression it exists to catch.
            assert_eq!(
                parts.len(),
                3,
                "a unique suffix lost a field; the counter is the last: {s}"
            );
            parts.remove(1);
            parts.join("-")
        })
        .collect();
    let total = without_clock.len();
    without_clock.sort();
    without_clock.dedup();
    assert_eq!(
        without_clock.len(),
        total,
        "suffixes collide once the clock is discounted: {drawn:?}"
    );
}

/// Same standard for the numeric rendering: the COUNTER field alone must be
/// distinct, so the assertion cannot be satisfied by the clock.
#[test]
fn nonce_is_unique_without_the_clocks_help() {
    let drawn: Vec<u128> = (0..16).map(|_| nonce()).collect();
    let mut counters: Vec<u128> = drawn.iter().map(|n| n >> 96).collect();
    let total = counters.len();
    counters.sort();
    counters.dedup();
    assert_eq!(
        counters.len(),
        total,
        "the nonce's counter field repeats, so the clock is doing the work: {drawn:?}"
    );
    // And the pid field is where the doc says it is, since a person reads
    // it out of a stale lock file.
    for n in drawn {
        assert_eq!(
            (n >> 64) & u32::MAX as u128,
            std::process::id() as u128,
            "the nonce's pid field moved"
        );
    }
}
