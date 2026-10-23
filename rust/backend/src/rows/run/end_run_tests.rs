use super::*;

fn report(phase: sot_log::SupervisorPhase) -> sot_log::attach_client::supervisor_client::StatusReport {
    sot_log::attach_client::supervisor_client::StatusReport { pid: 1, created: 1, phase, run: None, voyage: None }
}

/// An authority that reports `Starting` (a held row is one) is waited out
/// for activation's bound of re-probes, then stopped once and judged by
/// the absence proof; a proof that finds a leg, or the fence still held,
/// keeps the row with an error naming the starting authority.
#[test]
fn a_still_starting_authority_is_stopped_then_proven() {
    use sot_log::SupervisorPhase as P;
    // Starting, Starting, Ready: settled after two pauses.
    let mut script = vec![P::Starting, P::Starting, P::Ready].into_iter();
    let mut pauses = 0;
    let settled = settle_starting(|| Ok(report(script.next().unwrap())), || pauses += 1);
    assert!(matches!(settled, Settle::Settled(ref s) if s.phase == P::Ready));
    assert_eq!(pauses, 2);

    // Starting throughout: still starting after exactly the activation bound of pauses.
    let mut pauses = 0;
    let settled = settle_starting(|| Ok(report(P::Starting)), || pauses += 1);
    assert!(matches!(settled, Settle::StillStarting));
    assert_eq!(pauses, super::super::activation::ACTIVATION_MAX_REPROBES);

    // The stop runs once; a proven absence is `Unheld`.
    let mut stops = 0;
    let got = end_still_starting(|| { stops += 1; Ok(()) }, || Ok(true));
    assert!(matches!(got, Ok(super::super::EndRunOutcome::Unheld)));
    assert_eq!(stops, 1);
    // A leg with no authority, and a fence the stopped authority did not release, keep the row.
    for (proof, words) in [(Ok(false), "stopped while starting"), (Err(NotProven::FenceUnavailable), "while starting")] {
        let err = end_still_starting(|| Ok(()), || proof).unwrap_err();
        assert!(err.to_string().contains(words), "{err}");
    }
    // A stop that failed is a kept row with its detail.
    let got = end_still_starting(|| Err("scope still holds a process".to_string()), || Ok(true));
    assert!(matches!(got, Ok(super::super::EndRunOutcome::NotEnded(ref d)) if d == "scope still holds a process"));
}

// Field defect (v0.6.0-rc.12): a supervisor that died out from under a row (e.g. a daemon-pair converge that ended the old
// build's supervisor) left the state dir holding only a lock FILE, and `end_run` used to treat every unreachable lane
// identically -- kept forever, with no way to tell a merely-unresponsive live holder from no holder at all. The proof is
// the supervisor fence and the row socket (the supervisor holds the fence for its whole life and its agent ends with
// it); no voyage is read.
#[test]
fn the_destroy_proof_needs_no_voyage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path();

    // A fence free and nothing answering the row's socket: proved, with no pointer and no voyage in the folder.
    match end_run(state_dir, "test reason", true) {
        Ok(super::EndRunOutcome::Unheld) => {}
        other => panic!("expected Ok(Unheld) with a free fence and no row socket: {other:?}"),
    }
    assert!(!state_dir.join("drawer.voyage").exists() && !state_dir.join("voyages").exists(), "the proof wrote a voyage");

    // Something else holds the SUPERVISOR fence right now: `end_run` keeps the unreachable-lane refusal (Err) rather
    // than fabricating `Unheld` out from under a live holder.
    let holder = sot_log::supervisor::journal::fence::lock_supervisor(state_dir).expect("take the fence");
    match end_run(state_dir, "test reason", true) {
        Err(_) => {}
        Ok(outcome) => {
            panic!("expected the unchanged unreachable-lane Err while the fence is held: {outcome:?}")
        }
    }
    drop(holder);
}
