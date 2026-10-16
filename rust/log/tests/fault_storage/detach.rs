//! The macOS teardown's detach policy: a refusal because the volume is busy is retried within a bound, because a
//! process outside the test (the first detach of a hosted job waits about 11 s) can hold the image for a while. The
//! policy takes its commands and clock as arguments, so the Linux build runs its test.

use std::time::Duration;

/// The longest a busy volume is retried, and the wait between tries.
pub const BOUND: Duration = Duration::from_secs(30);
pub const PAUSE: Duration = Duration::from_secs(1);

/// What one `hdiutil detach` answered.
pub struct Attempt {
    pub ok: bool,
    /// Exit 16 or "Resource busy": the only refusal that is retried.
    pub busy: bool,
    pub report: String,
}

/// Detach until it succeeds. A busy refusal logs its report and the volume's holders, and is retried until `bound`
/// has passed; the failure then names the last refusal and the holders. Any other refusal fails at once. Never forces.
pub fn detach_within(
    mut detach: impl FnMut() -> Attempt,
    mut holders: impl FnMut() -> String,
    mut sleep: impl FnMut(Duration),
    mut elapsed: impl FnMut() -> Duration,
    bound: Duration,
) -> Result<u32, String> {
    let mut tries = 0;
    loop {
        tries += 1;
        let attempt = detach();
        if attempt.ok {
            return Ok(tries);
        }
        if !attempt.busy {
            return Err(format!("detach refused:\n{}", attempt.report));
        }
        let holding = holders();
        println!(
            "L3 fixture macos detach busy try={tries}\n{}\nholders:\n{holding}",
            attempt.report
        );
        if elapsed() >= bound {
            return Err(format!(
                "detach still busy after {tries} tries within {bound:?}; last refusal:\n{}\nholders:\n{holding}",
                attempt.report
            ));
        }
        sleep(PAUSE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn refused() -> Attempt {
        Attempt {
            ok: false,
            busy: true,
            report: "couldn't eject \"disk7\" - Resource busy".into(),
        }
    }

    #[test]
    fn a_busy_volume_is_retried_until_it_detaches() {
        let calls = Cell::new(0);
        let tries = detach_within(
            || {
                calls.set(calls.get() + 1);
                if calls.get() <= 2 {
                    refused()
                } else {
                    Attempt {
                        ok: true,
                        busy: false,
                        report: String::new(),
                    }
                }
            },
            || "mds 123".into(),
            |_| {},
            || Duration::ZERO,
            BOUND,
        );
        assert_eq!(tries, Ok(3));
    }

    #[test]
    fn a_volume_busy_past_the_bound_fails_naming_the_holders() {
        let clock = Cell::new(Duration::ZERO);
        let error = detach_within(
            refused,
            || "mds 123 user cwd /vol".into(),
            |pause| clock.set(clock.get() + pause),
            || clock.get(),
            BOUND,
        )
        .unwrap_err();
        assert!(
            error.contains("Resource busy") && error.contains("mds 123 user cwd /vol"),
            "{error}"
        );
        assert!(error.contains("31 tries"), "{error}");
    }

    #[test]
    fn another_refusal_fails_at_once() {
        let calls = Cell::new(0);
        let error = detach_within(
            || {
                calls.set(calls.get() + 1);
                Attempt {
                    ok: false,
                    busy: false,
                    report: "not mounted".into(),
                }
            },
            String::new,
            |_| panic!("no wait for a refusal that is not busy"),
            || Duration::ZERO,
            BOUND,
        )
        .unwrap_err();
        assert_eq!((calls.get(), error.contains("not mounted")), (1, true));
    }
}
