use super::*;

fn found(fail: Option<&str>, steps: &[&str]) -> Found {
    Found { fail: fail.map(String::from), steps: steps.iter().map(|s| (*s).to_string()).collect(), pass: "Ship of Tools 1.2.3 at 0123abcde on this computer".into(), ..Found::default() }
}

#[test]
fn checked_results_join_only_nonempty_verdict_parts() {
    let mut f = Found::default();
    f.checked("publish guard scope: known clones only");
    f.checked("");
    f.checked("publish guard checked base: ready");
    let checked = "publish guard scope: known clones only; publish guard checked base: ready";
    assert_eq!(line(&f, ""), (format!("PASS: {checked}"), 0));
    assert_eq!(line(&f, "; pinned"), (format!("PASS: {checked}; pinned"), 0));
    f.pass = "installed".into();
    assert_eq!(line(&f, ""), (format!("PASS: installed; {checked}"), 0));
    f.step("");
    f.step("install Julia");
    f.step("");
    assert_eq!(line(&f, "; pinned"), (format!("ONE STEP LEFT: install Julia; {checked}; pinned"), 0));
    assert_eq!(line(&Found::default(), "; pinned"), ("PASS: pinned".into(), 0));
}

/// One verdict, in its words: a run that reached its checks ends PASS, ONE STEP LEFT (both exit 0, the install is whole)
/// or FAIL (exit 1), and a pinned run's line says where it is pinned whichever it is; a stop is a FAIL naming its
/// check, with its own exit code (2 for a refusal) and the tail the run had written; a stop that lands while a step is
/// being started still ends that step, so none outlives the stop. That a signal ends in one line is the entry suite's
/// (installer-entry.sh, a TERM mid-step).
#[test]
fn a_run_ends_in_one_verdict() {
    assert_eq!(line(&found(None, &[]), ""), ("PASS: Ship of Tools 1.2.3 at 0123abcde on this computer".into(), 0));
    assert_eq!(line(&found(None, &["a", "b"]), ""), ("ONE STEP LEFT: a; then b".into(), 0));
    assert_eq!(line(&found(Some("comm: stale"), &["a"]), ""), ("FAIL: comm: stale".into(), 1));
    let pin = pin_tail("1.2.3");
    for (f, word) in [(found(None, &[]), "PASS: "), (found(None, &["a"]), "ONE STEP LEFT: "), (found(Some("x: y"), &[]), "FAIL: ")] {
        let (text, _) = line(&f, &pin);
        assert!(text.starts_with(word) && text.ends_with("; pinned at 1.2.3: automatic updates stay off until a run that names no version"), "{text}");
    }

    let stop = Stop::at("options", 2, "unknown flag: --x");
    assert_eq!(stop_line(&stop, ""), ("FAIL: options: unknown flag: --x".into(), 2));
    let stop = Stop::at("stage", 1, "no room");
    assert_eq!(stop_line(&stop, "; the update to v1 stays armed: the next start applies it").0, "FAIL: stage: no room; the update to v1 stays armed: the next start applies it");

    // Steps start back to back; the stop lands among them, after the flag that makes every later step a long one. A
    // step the stop missed would be alive after it.
    #[cfg(unix)]
    for _ in 0..100 {
        let steps: &'static super::super::cmd::Steps = Box::leak(Box::new(super::super::cmd::Steps::new()));
        let dir = tempfile::tempdir().unwrap();
        let (pids, flag) = (dir.path().join("pids"), dir.path().join("flag"));
        let (p, f) = (pids.clone(), flag.clone());
        std::thread::spawn(move || loop {
            steps.output(std::process::Command::new("sh").args(["-c", "[ -e \"$2\" ] || exit 0; echo $$ >> \"$1\"; exec sleep 30", "sh"]).arg(&p).arg(&f));
        });
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&flag, "").unwrap();
        steps.stop(libc::SIGTERM, std::time::Duration::from_secs(2));
        std::thread::sleep(std::time::Duration::from_millis(50));
        for pid in std::fs::read_to_string(&pids).unwrap_or_default().lines().filter_map(|l| l.trim().parse::<i32>().ok()) {
            // SAFETY: signal 0 probes a pid this test's step wrote; it sends nothing.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            assert!(!alive, "a step started as the stop landed outlived it");
        }
    }
}
