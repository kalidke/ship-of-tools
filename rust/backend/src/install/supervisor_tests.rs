use super::*;

/// A converge is applied only once the apply said the tag applied and the launch said DONE after it; a supervisor whose
/// start line names no code id gets no converge.
#[test]
fn a_converge_is_done_only_after_the_apply_and_the_launch_say_so() {
    assert_eq!(converge_state("", "v1"), Converge::Waiting);
    assert_eq!(converge_state("status: DONE\nsot-apply: APPLIED v1 (previous kept)\n", "v1"), Converge::Waiting);
    assert_eq!(converge_state("sot-apply: APPLIED v1 (previous kept)\nstatus: Starting\nstatus: DONE\n", "v1"), Converge::Done);
    assert_eq!(converge_state("sot-apply: APPLIED v0 (x)\nstatus: DONE\n", "v1"), Converge::Waiting);
    assert_eq!(converge_state("sot-apply: installing sotd failed - restoring previous binaries and pointers\n", "v1"), Converge::Restored);
    let log = "t  pid=7  supervisor start (relaunched=False) depth=0 ws=90MB code=AAA-BBB\nt  pid=9  supervisor start (relaunched=False) depth=0 ws=90MB code=CCC\nt  pid=7  supervisor start (relaunched=True) depth=1 ws=91MB code=DDD\n";
    assert_eq!(start_code(log, 7), "DDD");
    assert_eq!(start_code(log, 9), "CCC");
    assert_eq!(start_code("t  pid=7  supervisor start (relaunched=False) depth=0 ws=90MB code=unknown\n", 7), "");
    assert_eq!(start_code("t  pid=77  supervisor start code=X\n", 7), "");
}

/// A converge wait ends when the apply and the launch say so after the request, on a restore, or at its bound by the
/// clock, and every end removes the sentinel; what the log said before the request does not count.
#[test]
fn a_converge_wait_ends_by_the_log_or_its_bound_and_takes_the_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    let (log, sentinel) = (dir.path().join("supervisor.log"), dir.path().join("relaunch.request"));
    let old = "sot-apply: APPLIED v1 (previous kept)\nstatus: DONE\n";
    let wait = |bound: Duration| {
        std::fs::write(&sentinel, "converge").unwrap();
        let t0 = Instant::now();
        (wait_converge(&log, old.len() as u64, &sentinel, "v1", bound, &|_| {}), t0.elapsed())
    };
    std::fs::write(&log, old).unwrap();
    let (r, took) = wait(Duration::from_secs(1));
    assert!(r.unwrap_err().contains("did not report v1 applied within 1 s"));
    assert!(took >= Duration::from_secs(1) && took < Duration::from_millis(1500), "{took:?}");
    assert!(!sentinel.exists());

    std::fs::write(&log, old).unwrap();
    let writer = {
        let log = log.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let mut text = std::fs::read_to_string(&log).unwrap();
            text.push_str("sot-apply: APPLIED v1 (previous kept)\nstatus: Starting\nstatus: DONE\n");
            std::fs::write(&log, text).unwrap();
        })
    };
    let (r, took) = wait(Duration::from_secs(20));
    writer.join().unwrap();
    assert!(r.is_ok() && took < Duration::from_secs(5), "{r:?} {took:?}");
    assert!(!sentinel.exists());

    std::fs::write(&log, format!("{old}sot-apply: installing sotd failed - restoring previous binaries and pointers\n")).unwrap();
    let (r, _) = wait(Duration::from_secs(20));
    assert!(r.unwrap_err().contains("did not complete"));
    assert!(!sentinel.exists());
}

/// An update applies one way: a supervisor that logs its code id converges; an older one, or a lone window, waits for
/// the person; with nothing running an ssh session leaves it for the desk and a desktop session applies it here.
#[test]
fn an_update_applies_one_way_by_what_runs() {
    assert_eq!(way(Some("AAA-BBB"), false, &|| Some(0)), Way::Converge);
    assert_eq!(way(Some(""), false, &|| Some(1)), Way::Quit);
    assert_eq!(way(None, true, &|| Some(0)), Way::Quit);
    assert_eq!(way(None, false, &|| Some(0)), Way::Desk);
    assert_eq!(way(None, false, &|| Some(2)), Way::Here);
    assert_eq!(way(None, false, &|| None), Way::Here, "a session that cannot be read is not taken for ssh");
}
