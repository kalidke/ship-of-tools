use super::*;

fn report(phase: sot_log::lane::wire::SupervisorPhase) -> sot_log::attach_client::supervisor_client::StatusReport {
    sot_log::attach_client::supervisor_client::StatusReport { pid: 1, created: 1, voyage: None, leg: None, phase }
}

/// An authority that reports `Starting` (a held row is one) is waited out
/// for activation's bound of re-probes, then stopped once and judged by
/// the absence proof; a proof that finds a leg, or the fence still held,
/// keeps the row with an error naming the starting authority.
#[test]
fn a_still_starting_authority_is_stopped_then_proven() {
    use sot_log::lane::wire::SupervisorPhase as P;
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

// Field defect (v0.6.0-rc.12): a supervisor that died out from under
// a row (e.g. a daemon-pair converge that ended the old build's
// supervisor) left the state dir holding only a lock FILE, and
// `end_run` used to treat every unreachable lane identically -- kept
// forever, with no way to tell a merely-unresponsive live holder
// from no holder at all. `query_status` fails against this temp dir
// in every case below (nothing is listening on its lane) -- decision
// 33 needs BOTH the fence AND the leg independently proven absent
// before `Unheld`; a missing pointer (nothing to check the leg
// against), a present leg, or a still-held fence each keep the row
// instead.
#[test]
fn end_run_is_unheld_only_when_both_the_fence_and_the_leg_are_proven_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path();

    // No supervisor.lock AND no published pointer at all -- the
    // fence is free, but there is nothing to prove the leg absent
    // against: uncertain, never fabricated as `Unheld`.
    match end_run(state_dir, "test reason", true) {
        Err(_) => {}
        Ok(outcome) => panic!("expected Err with no pointer to check the leg against: {outcome:?}"),
    }

    // A published pointer naming a real voyage root whose own
    // `writer.lock` exists and is free -- BOTH halves now proven
    // absent.
    let voyage_id = "a1b2c3d4-e5f6-4890-9abc-def012345678";
    sot_log::supervisor::journal::pointer::publish(state_dir, voyage_id).expect("publish the pointer");
    let voyage_root = sot_log::supervisor::voyage_root_path(state_dir, voyage_id);
    std::fs::create_dir_all(&voyage_root).expect("voyage root");
    std::fs::write(voyage_root.join("writer.lock"), b"").expect("writer.lock file");
    match end_run(state_dir, "test reason", true) {
        Ok(super::EndRunOutcome::Unheld) => {}
        other => panic!("expected Ok(Unheld) with no authority and no leg: {other:?}"),
    }

    // A leg holds the voyage's own `writer.lock` -- reported
    // instead of silently orphaned.
    let leg = sot_log::lock_writer(&voyage_root.join("writer.lock")).expect("take the writer lock");
    match end_run(state_dir, "test reason", true) {
        Err(_) => {}
        Ok(outcome) => panic!("expected Err while a leg holds the row with no authority: {outcome:?}"),
    }
    drop(leg);

    // Something else holds the SUPERVISOR fence right now -- the
    // lock attempt must fail regardless of the leg, so `end_run`
    // keeps the unreachable-lane refusal (Err) rather than
    // fabricating `Unheld` out from under a live holder.
    let holder = sot_log::supervisor::journal::fence::lock_supervisor(state_dir).expect("take the fence");
    match end_run(state_dir, "test reason", true) {
        Err(_) => {}
        Ok(outcome) => {
            panic!("expected the unchanged unreachable-lane Err while the fence is held: {outcome:?}")
        }
    }
    drop(holder);
}

// The direct writer.lock cases (`leg_absent`'s own Ok(true)/Ok(false)
// on a real held/free lock) are already exercised through `end_run`
// by `end_run_is_unheld_only_when_both_the_fence_and_the_leg_are_proven_absent`
// above (including the no-pointer-published Err case) -- this test
// instead targets what `leg_absent` cannot organically produce on
// Linux at all: the OTHER, non-contention refusal `lock_writer` can
// report (Windows' reparse-point check, `host/`) sharing the SAME
// `Error::State` shape as genuine contention. `is_lock_contention` is
// the pure predicate that tells them apart (Codex review,
// 2026-09-11); this is its regression test -- pure string matching,
// independent of any real lock file.
#[test]
fn lock_contention_is_recognized_only_by_its_own_message() {
    assert!(
        is_lock_contention("lock held by another process: \"/tmp/x/writer.lock\""),
        "lock_writer's own bounded-retry-exhaustion text must be recognized as contention"
    );
    assert!(
        !is_lock_contention(
            "writer.lock at \"/tmp/x/writer.lock\" is a reparse point — refusing a redirected fence"
        ),
        "a reparse-point refusal is not contention -- absence must stay unproven, not read as \"a leg lives\""
    );
    assert!(!is_lock_contention("some unrelated State error"));
}
