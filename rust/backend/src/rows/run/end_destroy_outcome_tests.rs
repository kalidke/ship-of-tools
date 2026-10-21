use super::*;

// An authority found ALREADY resting in `EndedNoRespawn` is
// `AlreadyEnded`, not a fabricated `RecordVerified` -- still
// `Removable` (safe to report "ended").
#[test]
fn already_ended_outcome_is_removable_and_distinct_from_record_verified() {
    let outcome =
        capsule_destroy_outcome_of(crate::rows::run::end_run::EndRunOutcome::AlreadyEnded);
    match outcome {
        CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
        CapsuleDestroyOutcome::Removable(detail) => {
            assert!(
                !detail.contains("verified"),
                "must not claim verification it never observed: {detail}"
            );
        }
        CapsuleDestroyOutcome::Kept { detail } => {
            panic!("AlreadyEnded is a confirmed end — must not be Kept: {detail}");
        }
    }
}

// Rounds out coverage of `capsule_destroy_outcome_of`'s remaining
// variants: a real end_run's own two confirmed outcomes both map to
// `Removable`, and `NotEnded` (failed/refused/outcome-unknown) maps
// to `Kept`.
#[test]
fn record_verified_and_closed_are_removable_not_ended_is_kept() {
    use crate::rows::run::end_run::EndRunOutcome as O;
    for outcome in [O::RecordVerified, O::RecordClosed] {
        assert!(
            matches!(
                capsule_destroy_outcome_of(outcome.clone()),
                CapsuleDestroyOutcome::Removable(_)
            ),
            "{outcome:?} must map to Removable"
        );
    }
    match capsule_destroy_outcome_of(O::NotEnded("end_run failed: boom".to_string())) {
        CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
        CapsuleDestroyOutcome::Kept { detail } => assert_eq!(detail, "end_run failed: boom"),
        CapsuleDestroyOutcome::Removable(detail) => {
            panic!("NotEnded must never map to Removable: {detail}");
        }
    }
}

// A leg that went `Terminal` (e.g. an unlaunchable agent argv) has no
// live run to orphan — `end_run` already sent it `stop` and waited
// for confirmed exit before ever reporting this outcome, so the row
// must be `Removable`, never stuck `Kept` forever (the gap this
// whole variant closes: an unendable capsule row).
#[test]
fn terminal_outcome_is_removable_not_kept() {
    use crate::rows::run::end_run::EndRunOutcome as O;
    match capsule_destroy_outcome_of(O::Terminal) {
        CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
        CapsuleDestroyOutcome::Removable(detail) => {
            assert!(
                detail.contains("terminal"),
                "detail should explain the row was terminal: {detail}"
            );
        }
        CapsuleDestroyOutcome::Kept { detail } => {
            panic!("Terminal is a confirmed end (stop was sent and awaited) — must not be Kept: {detail}");
        }
    }
}

// `Unheld` (no supervisor holds the row — see its own doc) is a
// confirmed end, same family as `Terminal`/`AlreadyEnded`: `Removable`,
// never `Kept`.
#[test]
fn unheld_outcome_is_removable_not_kept() {
    use crate::rows::run::end_run::EndRunOutcome as O;
    match capsule_destroy_outcome_of(O::Unheld) {
        CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
        CapsuleDestroyOutcome::Removable(detail) => {
            assert_eq!(detail, "no supervisor held the row");
        }
        CapsuleDestroyOutcome::Kept { detail } => {
            panic!("Unheld means nobody holds the row — must not be Kept: {detail}");
        }
    }
}

// A `Kept` outcome must build the SAME typed error the non-default
// path returns, and must NEVER signal a `run_ended` broadcast.
#[test]
fn kept_outcome_builds_the_typed_error_and_never_broadcasts_run_ended() {
    let (payload, confirmed_ended) = default_row_end_response(
        "ws-local-1",
        "local",
        "local",
        CapsuleDestroyOutcome::Kept {
            detail: "supervisor is starting; retry".to_string(),
        },
    );
    assert_eq!(
        payload.get("code").and_then(|v| v.as_str()),
        Some("capsule_end_not_reached")
    );
    assert!(payload.get("error").is_some());
    assert!(
        payload.get("workspace_id").is_none(),
        "must not carry the success shape's own fields: {payload:?}"
    );
    assert!(payload.get("kept").is_none());
    assert!(
        !confirmed_ended,
        "a Kept outcome must never signal a run_ended broadcast"
    );
}

// The mirror case: a `Removable` (confirmed) outcome DOES build the
// success shape and DOES signal the broadcast.
#[test]
fn removable_outcome_builds_success_and_signals_run_ended() {
    let (payload, confirmed_ended) = default_row_end_response(
        "ws-local-1",
        "local",
        "local",
        CapsuleDestroyOutcome::Removable("run ended and verified".to_string()),
    );
    assert!(
        payload.get("error").is_none(),
        "must not error: {payload:?}"
    );
    assert_eq!(
        payload.get("workspace_id").and_then(|v| v.as_str()),
        Some("ws-local-1")
    );
    assert!(payload.get("kept").and_then(|v| v.as_str()).is_some());
    assert!(confirmed_ended);
}
