use super::*;

fn found(fail: Option<&str>, steps: &[&str]) -> Found {
    Found { fail: fail.map(String::from), steps: steps.iter().map(|s| (*s).to_string()).collect(), pass: "Ship of Tools 1.2.3 at 0123abcde on this computer".into() }
}

/// One verdict, in its words: a run that reached its checks ends PASS, ONE STEP LEFT (both exit 0, the install is whole)
/// or FAIL (exit 1), and a pinned run's line says where it is pinned whichever it is; a stop is a FAIL naming its
/// check, with its own exit code (2 for a refusal) and the tail the run had written. That a signal ends in one line is
/// the entry suite's (installer-entry.sh, a TERM mid-step).
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
}
