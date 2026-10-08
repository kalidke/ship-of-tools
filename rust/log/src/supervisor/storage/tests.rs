use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

fn storage_error() -> std::io::Error {
    #[cfg(unix)]
    return std::io::Error::from_raw_os_error(libc::ENOSPC);
    #[cfg(windows)]
    return std::io::Error::from_raw_os_error(112);
}

// The scripted probe of `the_wait_backs_off_and_resumes`; no other test uses it.
static SCRIPT: Mutex<VecDeque<std::io::Result<()>>> = Mutex::new(VecDeque::new());
static PROBES: AtomicUsize = AtomicUsize::new(0);

fn scripted_probe(_: &Path) -> std::io::Result<()> {
    PROBES.fetch_add(1, Ordering::SeqCst);
    SCRIPT
        .lock()
        .unwrap()
        .pop_front()
        .expect("a scripted probe result")
}

fn script(results: impl IntoIterator<Item = std::io::Result<()>>) {
    *SCRIPT.lock().unwrap() = results.into_iter().collect();
}

/// Ticks at `at` until the in-flight probe has finished, then returns that
/// tick's outcome; the probe worker is a thread, so its result arrives on a
/// later tick of the same instant.
fn finish_probe(mut wait: Wait, step: &mut u32, at: Instant) -> Outcome {
    for _ in 0..2000 {
        match advance(wait, step, Path::new("unused"), at) {
            Outcome::Waiting(w) if w.in_flight.is_some() => wait = w,
            other => return other,
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the probe never finished");
}

/// The tick at `at` must start no probe.
fn no_probe(wait: Wait, step: &mut u32, at: Instant) -> Wait {
    let before = PROBES.load(Ordering::SeqCst);
    let Outcome::Waiting(wait) = advance(wait, step, Path::new("unused"), at) else {
        panic!("expected to keep waiting");
    };
    assert!(
        wait.in_flight.is_none() && PROBES.load(Ordering::SeqCst) == before,
        "no probe is due yet"
    );
    wait
}

/// Waits for the due probe to start and finish; returns the outcome.
fn probe_at(wait: Wait, step: &mut u32, at: Instant) -> Outcome {
    let Outcome::Waiting(wait) = advance(wait, step, Path::new("unused"), at) else {
        panic!("the probe starts as a worker");
    };
    assert!(wait.in_flight.is_some(), "a probe is due and starts");
    finish_probe(wait, step, at)
}

/// The scripted probe has one script, so the parts run in this order, in one test.
#[test]
fn the_wait_backs_off_and_resumes() {
    let at = backs_off_then_resumes(Instant::now());
    let at = ends_terminal_or_not_storage(at);
    one_probe_at_a_time_and_bounded(at);
}

fn backs_off_then_resumes(t0: Instant) -> Instant {
    let mut step = 0;
    let full = || Err(storage_error());
    script([full(), full(), full(), full(), full(), full(), Ok(())]);

    // Probes at +1, +2, +4, +8, +16, +30 s after each failure, then a success.
    let mut at = t0;
    let mut wait = no_probe(
        Wait::with_probe(Resume::Respawn, scripted_probe),
        &mut step,
        at,
    );
    for (expected_step, seconds) in [(1, 1), (2, 2), (3, 4), (4, 8), (5, 16), (6, 30)] {
        wait = no_probe(
            wait,
            &mut step,
            at + Duration::from_millis(seconds * 1000 - 1),
        );
        at += Duration::from_secs(seconds);
        let Outcome::Waiting(next) = probe_at(wait, &mut step, at) else {
            panic!("a probe that meets storage exhaustion waits again");
        };
        assert_eq!(step, expected_step);
        wait = next;
    }
    assert_eq!(PROBES.load(Ordering::SeqCst), 6);

    // The seventh probe is another 30 s on and succeeds.
    let wait = no_probe(wait, &mut step, at + Duration::from_secs(29));
    at += Duration::from_secs(30);
    match probe_at(wait, &mut step, at) {
        Outcome::Resume(Resume::Respawn) => {}
        _ => panic!("a successful probe resumes"),
    }
    assert_eq!(step, 6, "the step is kept across a resume");

    // A later wait continues from the kept step; a stable leg returns it to 0.
    script([Ok(())]);
    let wait = no_probe(
        Wait::with_probe(Resume::Respawn, scripted_probe),
        &mut step,
        at,
    );
    let at = at + delay(step);
    assert!(matches!(
        probe_at(wait, &mut step, at),
        Outcome::Resume(Resume::Respawn)
    ));
    let (mut counter, mut kept) = (2, 6);
    account(&mut counter, &mut kept, false);
    assert_eq!((counter, kept), (0, 0));
    account(&mut counter, &mut kept, true);
    assert_eq!((counter, kept), (1, 0));
    at
}

fn ends_terminal_or_not_storage(at: Instant) -> Instant {
    let mut step = 6;

    // An unrecognized error ends the wait Terminal.
    script([Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "denied",
    ))]);
    let wait = no_probe(
        Wait::with_probe(Resume::Respawn, scripted_probe),
        &mut step,
        at,
    );
    let at = at + delay(step);
    assert!(matches!(
        probe_at(wait, &mut step, at),
        Outcome::Terminal(_)
    ));

    // A suspected death probes at once; a pass is not storage.
    let mut step = 0;
    script([Ok(())]);
    let outcome = probe_at(
        Wait::with_probe(Resume::Suspect, scripted_probe),
        &mut step,
        at,
    );
    assert!(matches!(outcome, Outcome::NotStorage));
    // ... and storage there is a known storage death, which waits and resumes as Respawn.
    script([Err(storage_error()), Ok(())]);
    let Outcome::Waiting(wait) = probe_at(
        Wait::with_probe(Resume::Suspect, scripted_probe),
        &mut step,
        at,
    ) else {
        panic!("a suspected death that meets storage exhaustion waits");
    };
    assert_eq!(step, 1);
    let wait = no_probe(wait, &mut step, at + delay(1) - Duration::from_millis(1));
    let at = at + delay(1);
    assert!(matches!(
        probe_at(wait, &mut step, at),
        Outcome::Resume(Resume::Respawn)
    ));
    at
}

fn one_probe_at_a_time_and_bounded(at: Instant) {
    // Never two probes at once: while a probe runs, a tick starts no second one.
    static STARTED: AtomicUsize = AtomicUsize::new(0);
    static RELEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    fn held_probe(_: &Path) -> std::io::Result<()> {
        STARTED.fetch_add(1, Ordering::SeqCst);
        while !RELEASE.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
    let mut step = 0;
    let Outcome::Waiting(wait) = advance(
        Wait::with_probe(Resume::Suspect, held_probe),
        &mut step,
        Path::new("."),
        at,
    ) else {
        panic!("the probe starts");
    };
    while STARTED.load(Ordering::SeqCst) == 0 {
        std::thread::sleep(Duration::from_millis(5));
    }
    let Outcome::Waiting(wait) =
        advance(wait, &mut step, Path::new("."), at + Duration::from_secs(1))
    else {
        panic!("still waiting on the running probe");
    };
    assert!(wait.in_flight.is_some());
    assert_eq!(
        STARTED.load(Ordering::SeqCst),
        1,
        "a second probe started while one ran"
    );

    // A probe still running past 60 s ends the wait Terminal.
    let Outcome::Terminal(detail) = advance(
        wait,
        &mut step,
        Path::new("."),
        at + Duration::from_secs(61),
    ) else {
        panic!("a probe past its bound is Terminal");
    };
    assert!(detail.contains("60"), "{detail}");
    RELEASE.store(true, Ordering::SeqCst);
}

#[test]
fn leg_death_reads_the_status_only() {
    assert_eq!(leg_death(Some(ExitStatus::Code(71))), LegDeath::Storage);
    assert_eq!(leg_death(Some(ExitStatus::Code(1))), LegDeath::Ordinary);
    assert_eq!(leg_death(Some(ExitStatus::Signal(9))), LegDeath::Ordinary);
    assert_eq!(leg_death(None), LegDeath::Unknown);
}

#[test]
fn delay_runs_one_to_sixteen_then_thirty_seconds() {
    let seconds: Vec<u64> = (0..8).map(|s| delay(s).as_secs()).collect();
    assert_eq!(seconds, [1, 2, 4, 8, 16, 30, 30, 30]);
}

#[test]
fn the_probe_cleans_up_only_its_own_file() {
    let dir = tempfile::tempdir().unwrap();
    let theirs = dir.path().join(".storage-probe-x");
    std::fs::write(&theirs, b"not ours").unwrap();
    let before: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    probe(dir.path()).unwrap();
    let mut after: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut before = before;
    before.sort();
    after.sort();
    assert_eq!(before, after, "the probe leaves no new entry");
    assert_eq!(
        std::fs::read(&theirs).unwrap(),
        b"not ours",
        "a name it did not create survives"
    );
}
