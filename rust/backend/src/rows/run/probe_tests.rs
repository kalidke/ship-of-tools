use super::*;

#[test]
fn phase_str_is_total_and_snake_case() {
    use sot_log::lane::wire::SupervisorPhase;
    assert_eq!(phase_str(SupervisorPhase::Starting), "starting");
    assert_eq!(phase_str(SupervisorPhase::Ready), "ready");
    assert_eq!(phase_str(SupervisorPhase::Ending), "ending");
    assert_eq!(phase_str(SupervisorPhase::EndedNoRespawn), "ended_no_respawn");
    assert_eq!(phase_str(SupervisorPhase::Terminal), "terminal");
}

#[test]
fn wire_constants_and_phase_str_are_the_phases_strings() {
    use crate::rows::workspace::Phase;
    use sot_log::lane::wire::SupervisorPhase;
    assert_eq!(UNREACHABLE_PHASE, "unreachable");
    assert_eq!(FOREIGN_PHASE, "foreign");
    assert_eq!(NEVER_STARTED_PHASE, "stopped");
    assert_eq!(UNREACHABLE_PHASE, Phase::Unreachable.as_wire_str());
    assert_eq!(FOREIGN_PHASE, Phase::Foreign.as_wire_str());
    assert_eq!(NEVER_STARTED_PHASE, Phase::Stopped.as_wire_str());
    for p in [
        SupervisorPhase::Starting,
        SupervisorPhase::Ready,
        SupervisorPhase::Ending,
        SupervisorPhase::EndedNoRespawn,
        SupervisorPhase::Terminal,
    ] {
        assert_eq!(phase_str(p), local_phase(p).as_wire_str());
    }
}

#[test]
fn never_started_phase_is_distinct_from_unreachable_and_every_answered_phase() {
    // First live shakedown fix: a capsule workspace nobody has ever
    // started must read as quietly "stopped", not as the loud
    // "unreachable" a query FAILURE reports — the two must never
    // collide with each other or with any answered lifecycle phase.
    use sot_log::lane::wire::SupervisorPhase;
    assert_ne!(NEVER_STARTED_PHASE, UNREACHABLE_PHASE);
    for p in [
        SupervisorPhase::Starting,
        SupervisorPhase::Ready,
        SupervisorPhase::Ending,
        SupervisorPhase::EndedNoRespawn,
        SupervisorPhase::Terminal,
    ] {
        assert_ne!(phase_str(p), NEVER_STARTED_PHASE);
    }
}

#[test]
fn phase_for_missing_pointer_only_fires_when_the_pointer_is_absent() {
    // Rule B: a published pointer means a supervisor reached a real
    // run at least once — that case defers to the real query (`None`)
    // rather than guessing; only a genuinely absent pointer
    // short-circuits to `NEVER_STARTED_PHASE`.
    assert_eq!(
        phase_for_missing_pointer(false),
        Some(NEVER_STARTED_PHASE)
    );
    assert_eq!(phase_for_missing_pointer(true), None);
}

#[test]
fn unreachable_phase_is_distinct_from_every_answered_phase() {
    use sot_log::lane::wire::SupervisorPhase;
    for p in [
        SupervisorPhase::Starting,
        SupervisorPhase::Ready,
        SupervisorPhase::Ending,
        SupervisorPhase::EndedNoRespawn,
        SupervisorPhase::Terminal,
    ] {
        assert_ne!(phase_str(p), UNREACHABLE_PHASE);
    }
}
