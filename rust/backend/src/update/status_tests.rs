use super::*;

fn found(version: &str) -> Chase {
    Chase::Found { version: version.into(), detail: String::new() }
}

fn preparing(version: &str) -> Chase {
    Chase::Preparing { version: version.into() }
}

fn failed(version: &str) -> Chase {
    Chase::NotApplied { version: version.into(), cause: "preparing update failed: no julia".into(), block: None }
}

fn fits(version: &str) -> Armed {
    Armed::Fits { version: version.into(), applies: applies(false, true).into() }
}

fn refused(version: Option<&str>) -> Armed {
    Armed::Refused { version: version.map(String::from), why: "armed over 0.7.2 but the install is now 0.7.3; it no longer fits".into() }
}

/// The state told is a pure function of the chase and the armed fact: a pointer that fits is ready unless this process
/// chases another release, which then comes first with the pointer's version beside it; a pointer the apply would
/// refuse is not applied unless a release is chased; else the chase's own phase. A release this process armed claims
/// nothing of its own, so it is found again, never up to date, when its pointer is gone. Each row is (chase, armed,
/// phase, version, detail, armed beside it).
#[test]
fn the_state_is_what_the_chase_and_the_armed_pointer_say() {
    use UpdatePhase as P;
    let unit = applies(false, true);
    let stale = "armed over 0.7.2 but the install is now 0.7.3; it no longer fits";
    let no_julia = "preparing update failed: no julia";
    let off = || Chase::Off { why: "dev build".into() };
    let armed_here = |version: &str| Chase::Armed { version: version.into() };
    let table = [
        // The chase alone.
        (Chase::Unchecked, Armed::Nothing, P::Unchecked, "", "", ""),
        (Chase::None, Armed::Nothing, P::None, "", "", ""),
        (found("0.7.4"), Armed::Nothing, P::Found, "0.7.4", "", ""),
        (preparing("0.7.4"), Armed::Nothing, P::Preparing, "0.7.4", "", ""),
        (failed("0.7.4"), Armed::Nothing, P::NotApplied, "0.7.4", no_julia, ""),
        (off(), Armed::Nothing, P::Off, "", "dev build", ""),
        (Chase::Blocked { why: "install.json cannot be read".into() }, Armed::Nothing, P::Blocked, "", "install.json cannot be read", ""),
        (Chase::CheckFailed { why: "check unavailable: timed out".into() }, Armed::Nothing, P::CheckFailed, "", "check unavailable: timed out", ""),
        // A pointer that fits is ready under every chase that is not about a release: after a restart, with checks
        // off (an operator's stage onto a dev install), with the record unreadable since, after a check that failed.
        (Chase::Unchecked, fits("0.7.4"), P::Ready, "0.7.4", unit, ""),
        (Chase::None, fits("0.7.4"), P::Ready, "0.7.4", unit, ""),
        (off(), fits("0.7.4"), P::Ready, "0.7.4", unit, ""),
        (Chase::Blocked { why: "x".into() }, fits("0.7.4"), P::Ready, "0.7.4", unit, ""),
        (Chase::CheckFailed { why: "x".into() }, fits("0.7.4"), P::Ready, "0.7.4", unit, ""),
        // A release being chased comes first when the pointer is for another version, which rides beside it.
        (found("0.7.5"), fits("0.7.4"), P::Found, "0.7.5", "", "0.7.4"),
        (preparing("0.7.5"), fits("0.7.4"), P::Preparing, "0.7.5", "", "0.7.4"),
        (failed("0.7.5"), fits("0.7.4"), P::NotApplied, "0.7.5", no_julia, "0.7.4"),
        // A release whose own pointer fits is ready in every phase of its chase: the daily check of an armed release
        // finds it, stages it and arms it again, and a reader is told ready throughout.
        (found("0.7.4"), fits("0.7.4"), P::Ready, "0.7.4", unit, ""),
        (preparing("0.7.4"), fits("0.7.4"), P::Ready, "0.7.4", unit, ""),
        (failed("0.7.4"), fits("0.7.4"), P::Ready, "0.7.4", unit, ""),
        // A release this process armed: the disk says the rest, and with no pointer it is found again, not up to date.
        (armed_here("0.7.4"), fits("0.7.4"), P::Ready, "0.7.4", unit, ""),
        (armed_here("0.7.4"), fits("0.7.5"), P::Ready, "0.7.5", unit, ""),
        (armed_here("0.7.4"), Armed::Nothing, P::Found, "0.7.4", ARMED_AND_GONE, ""),
        (armed_here("0.7.4"), refused(Some("0.7.4")), P::NotApplied, "0.7.4", stale, ""),
        // A pointer the apply would refuse is not applied, with the judge's words, under every chase not about a release;
        // one that cannot be read has no version.
        (Chase::Unchecked, refused(Some("0.7.4")), P::NotApplied, "0.7.4", stale, ""),
        (Chase::None, refused(Some("0.7.4")), P::NotApplied, "0.7.4", stale, ""),
        (off(), refused(None), P::NotApplied, "", stale, ""),
        // A refused pointer is not ready meanwhile.
        (preparing("0.7.5"), refused(Some("0.7.4")), P::Preparing, "0.7.5", "", ""),
        (failed("0.7.5"), refused(Some("0.7.4")), P::NotApplied, "0.7.5", no_julia, ""),
    ];
    for (chase, armed, phase, version, detail, beside) in table {
        let said = told(&chase, &armed, Some(7));
        let want = UpdateState { phase, version: version.into(), detail: detail.into(), armed: beside.into(), checked_s: Some(7), unread: false };
        assert_eq!(said, want, "{chase:?} with {armed:?}");
        assert!(!(said.phase == P::None && !matches!(chase, Chase::None)), "up to date is said only by a check that found nothing: {chase:?}");
    }
}

/// The words of what applies a ready update are read in `sotd status`, in a row about another computer, and on a
/// computer that runs a daemon and no window: each names the daemon's computer from any reader's seat, and none says
/// here or this computer.
#[test]
fn the_ready_words_read_right_in_a_row_about_another_computer() {
    assert_eq!(applies(false, true), "applies when that computer's sotd.service next starts (a start of Ship of Tools there, or a restart of the service)");
    assert_eq!(applies(false, false), "applies when Ship of Tools is next started on that computer");
    assert_eq!(applies(true, true), "applies at that daemon's daily check with no client attached, or at its next start");
    assert_eq!(applies(true, false), applies(true, true), "auto mode says its own, with or without a unit");
    for words in [applies(false, true), applies(false, false), applies(true, true)] {
        let lower = format!(" {} ", words.to_lowercase());
        assert!(!lower.contains(" here ") && !lower.contains("this computer") && !lower.contains("window"), "{words}");
    }
}

/// A write that changes nothing a reader is told wakes no reader: the same chase again, a check that ends as the last
/// one did (only its instant moves), and a reading of the disk that finds what the last one found.
#[tokio::test]
async fn a_state_written_again_unchanged_wakes_no_reader() {
    let status = Status::new(Chase::Unchecked, false, || None);
    let mut reader = status.0.facts.subscribe();
    // Whether a write since the reader last looked woke it; looking marks what is there as seen.
    let woken = |reader: &mut watch::Receiver<Facts>| {
        let woken = reader.has_changed().unwrap();
        reader.borrow_and_update();
        woken
    };
    assert!(!woken(&mut reader), "nothing written yet");

    status.set_chase(preparing("0.7.4"));
    assert!(woken(&mut reader), "a new state wakes the reader");
    status.set_chase(preparing("0.7.4"));
    assert!(!woken(&mut reader), "the same step written again");

    status.checked(Chase::None);
    assert!(woken(&mut reader));
    assert!(status.state().checked_s.is_some(), "the check's instant is kept");
    status.checked(Chase::None);
    assert!(!woken(&mut reader), "a check that ends as the last one did");

    status.reconcile().await;
    assert!(!woken(&mut reader), "a disk that holds what it held");
    assert_eq!(status.state().phase, UpdatePhase::None);
}

/// A reading asked for while another hangs is begun when that one ends, though its caller stopped waiting: a pointer
/// armed after the hung reading began is in memory once the readings end, with nothing asking again. Before this a
/// caller behind a reading in flight left with no reading of its own, and memory kept the fact from before the arm.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reading_asked_for_behind_one_that_hangs_is_begun_when_it_ends() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let updates = dir.path().join("updates");
    std::fs::create_dir(&updates).unwrap();
    let target = "linux-x86_64";
    // The first reading sees a disk with nothing armed and then hangs; every later one reads the folder.
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (held, begun) = (std::sync::Mutex::new(held), Arc::new(AtomicUsize::new(0)));
    let status = Status::new(Chase::Unchecked, false, {
        let (updates, begun) = (updates.clone(), begun.clone());
        move || {
            if begun.fetch_add(1, Ordering::SeqCst) == 0 {
                let _ = held.lock().unwrap().recv();
                return None;
            }
            Some(Disk { updates: updates.clone(), target: target.into(), unit: false })
        }
    });
    let first = tokio::spawn({
        let status = status.clone();
        async move { status.reconcile().await }
    });
    while begun.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    std::fs::write(
        updates.join(format!("pending-{target}.json")),
        format!(
            r#"{{"schema":1,"repo":"kalidke/ship-of-tools","tag":"v9.9.9","version":"9.9.9","target":"{target}",
                "asset":"sot-9.9.9-{target}.tar.gz","asset_sha256":"{}","checkout":"/x","commit":"cc","armed_at":1}}"#,
            "ab".repeat(32)
        ),
    )
    .unwrap();
    assert!(!status.reconcile().await, "the arm's reading waited behind the hung one and its caller left");
    assert!(!first.await.unwrap());
    assert_eq!(begun.load(Ordering::SeqCst), 1, "a second reading was begun beside the one that hangs");

    drop(release);
    let began = Instant::now();
    while status.state().phase != UpdatePhase::Ready {
        assert!(began.elapsed() < Duration::from_secs(5), "memory kept the fact from before the arm: {:?}", status.state());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(status.state().version, "9.9.9");
    assert_eq!(begun.load(Ordering::SeqCst), 2, "one reading for the call that left, and no more");
}
