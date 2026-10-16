//! The macOS teardown's detach policy: the image's disk is detached again while `hdiutil info` still lists it, because
//! a process outside the test (the first detach of a hosted job waits about 11 s) can hold the image for a while, and a
//! refused eject can come after the unmount succeeded, when the mount path no longer names the image. The policy
//! takes its commands and clock as arguments, so the Linux build runs its test.

use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

/// The longest after the first try that a new try may start, and the wait between tries. A try already running is
/// not cut short, so the worst case is the bound plus one try's own bounds.
pub const BOUND: Duration = Duration::from_secs(30);
pub const PAUSE: Duration = Duration::from_secs(1);

/// What one `hdiutil detach` answered.
pub struct Attempt {
    pub ok: bool,
    pub report: String,
}

/// The whole-disk device `hdiutil attach` printed: the first token of its first line, `/dev/diskN`.
pub fn attached_device(attach_output: &str) -> Option<String> {
    let token = attach_output.lines().next()?.split_whitespace().next()?;
    let digits = token.strip_prefix("/dev/disk")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| token.to_owned())
}

/// Whether `hdiutil info` still lists `device`: a line that starts with it as a whole token.
pub fn lists_device(info: &str, device: &str) -> bool {
    info.lines()
        .any(|line| line.split_whitespace().next() == Some(device))
}

/// A diagnostic command's exit and output, or why it has none: a command that cannot start or overruns `bound` is
/// text for the log, never a panic, because a diagnostic must not replace the refusal it explains.
pub fn run_diagnostic(
    mut command: Command,
    bound: Duration,
) -> Result<(ExitStatus, String, String), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .map_err(|error| format!("could not start: {error}"))?;
    sot_log::test_isolated::drain(child).try_wait_within(bound)
}

/// Whether an `lsof` listing names `pid` as a holder: a line whose second field, the PID column, is it.
pub fn names_pid(listing: &str, pid: u32) -> bool {
    let pid = pid.to_string();
    listing
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(pid.as_str()))
}

/// Detach `device` until `hdiutil info` no longer lists it. Each refusal logs its report and the holders, whatever the
/// refusal says: success is the state, not hdiutil's words. A listing that names `own_pid` fails at once, since the
/// test itself holds the image and waiting would only hide the leak. Otherwise tries stop starting once `bound` has
/// passed; the failure then names the last refusal and the holders. Never forces.
pub fn detach_within(
    device: &str,
    mut detach: impl FnMut(&str) -> Attempt,
    mut attached: impl FnMut() -> bool,
    mut holders: impl FnMut() -> String,
    mut sleep: impl FnMut(Duration),
    mut elapsed: impl FnMut() -> Duration,
    bound: Duration,
    own_pid: u32,
) -> Result<u32, String> {
    let mut tries = 0;
    loop {
        tries += 1;
        let attempt = detach(device);
        if !attached() {
            return Ok(tries);
        }
        println!(
            "L3 fixture macos detach refused try={tries} ok={}\n{}",
            attempt.ok, attempt.report
        );
        let holding = holders();
        println!("holders:\n{holding}");
        if names_pid(&holding, own_pid) {
            return Err(format!(
                "the test's own process {own_pid} holds {device}; last refusal:\n{}\nholders:\n{holding}",
                attempt.report
            ));
        }
        if elapsed() >= bound {
            return Err(format!(
                "{device} still attached after {tries} tries within {bound:?}; last refusal:\n{}\nholders:\n{holding}",
                attempt.report
            ));
        }
        sleep(PAUSE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// The attach lines of the hosted run that failed, and its refusal.
    const ATTACH: &str = "/dev/disk7          \tGUID_partition_scheme          \t\n/dev/disk7s1        \tApple_APFS                     \t\n/dev/disk8          \tEF57347C-0000-11AA-AA11-0030654\t\n/dev/disk8s1        \t41504653-0000-11AA-AA11-0030654\t/tmp/l3 premise x/volume\n";
    const OWN: u32 = 4242;
    const REFUSAL: &str = "hdiutil: couldn't eject \"disk7\" - Resource busy";

    fn refused() -> Attempt {
        Attempt {
            ok: false,
            report: format!("command status=exit status: 16 code=Some(16)\ncommand stderr:\n{REFUSAL}"),
        }
    }

    #[test]
    fn a_diagnostic_that_cannot_run_or_overruns_is_text_not_a_panic() {
        let mut quick = Command::new("echo");
        quick.arg("holder");
        let (status, out, _) = run_diagnostic(quick, Duration::from_secs(20)).unwrap();
        assert!(status.success() && out.contains("holder"));
        let missing = Command::new("/nonexistent/lsof");
        assert!(run_diagnostic(missing, Duration::from_secs(1)).unwrap_err().contains("could not start"));
        let mut slow = Command::new("sleep");
        slow.arg("30");
        assert!(run_diagnostic(slow, Duration::from_millis(200)).is_err());
    }

    #[test]
    fn the_device_is_the_first_disk_of_the_attach_output() {
        assert_eq!(attached_device(ATTACH).as_deref(), Some("/dev/disk7"));
        assert_eq!(attached_device("hdiutil: attach failed\n"), None);
        assert_eq!(attached_device("/dev/disk7s1\tApple_APFS\n"), None);
        assert_eq!(attached_device(""), None);
    }

    #[test]
    fn info_lists_a_device_by_whole_token() {
        let info = "image-path   : /tmp/volume.dmg\n/dev/disk7          \tGUID_partition_scheme\n/dev/disk7s1        \tApple_APFS\n";
        assert!(lists_device(info, "/dev/disk7"));
        assert!(!lists_device(info, "/dev/disk"));
        assert!(!lists_device("/dev/disk70\tx\n", "/dev/disk7"));
        assert!(!lists_device("framework : 600\n", "/dev/disk7"));
    }

    /// The red of run 38927663276: the unmount succeeded and the eject was refused with exit 16. The mount path no
    /// longer names the image, so the retry goes to the device and succeeds once `hdiutil info` stops listing it.
    #[test]
    fn a_refused_eject_after_the_unmount_is_retried_against_the_device() {
        let targets = RefCell::new(Vec::new());
        let busy = Cell::new(true);
        let tries = detach_within(
            "/dev/disk7",
            |target| {
                targets.borrow_mut().push(target.to_owned());
                if targets.borrow().len() == 1 {
                    refused()
                } else {
                    busy.set(false);
                    Attempt {
                        ok: true,
                        report: String::new(),
                    }
                }
            },
            || busy.get(),
            || "no holder".into(),
            |_| {},
            || Duration::ZERO,
            BOUND,
            OWN,
        );
        assert_eq!(tries, Ok(2));
        assert_eq!(*targets.borrow(), ["/dev/disk7", "/dev/disk7"]);
    }

    #[test]
    fn a_refusal_that_in_fact_detached_is_success() {
        let tries = detach_within(
            "/dev/disk7",
            |_| refused(),
            || false,
            String::new,
            |_| panic!("no wait once the image is gone"),
            || Duration::ZERO,
            BOUND,
            OWN,
        );
        assert_eq!(tries, Ok(1));
    }

    #[test]
    fn a_holder_that_is_the_test_itself_fails_at_once() {
        let listing = "COMMAND  PID USER   FD   TYPE DEVICE SIZE/OFF NODE NAME\nmds      123 root    5r   BLK  1,20      0t0  900 /dev/disk7\nfault_st 4242 runner  9u   BLK  1,20      0t0  900 /dev/disk7\n";
        assert!(names_pid(listing, 4242) && !names_pid(listing, 424) && !names_pid(listing, 900));
        let calls = Cell::new(0);
        let error = detach_within(
            "/dev/disk7",
            |_| {
                calls.set(calls.get() + 1);
                refused()
            },
            || true,
            || listing.into(),
            |_| panic!("no wait for a leak by the test"),
            || Duration::ZERO,
            BOUND,
            OWN,
        )
        .unwrap_err();
        assert!(calls.get() == 1 && error.contains("own process 4242"), "{error}");
    }

    #[test]
    fn an_image_attached_past_the_bound_fails_naming_the_holders() {
        let clock = Cell::new(Duration::ZERO);
        let error = detach_within(
            "/dev/disk7",
            |_| refused(),
            || true,
            || "mds 123 user cwd /vol".into(),
            |pause| clock.set(clock.get() + pause),
            || clock.get(),
            BOUND,
            OWN,
        )
        .unwrap_err();
        assert!(
            error.contains("Resource busy") && error.contains("mds 123 user cwd /vol"),
            "{error}"
        );
        assert!(error.contains("31 tries"), "{error}");
    }
}
