use super::*;

fn key(b: u8) -> [u8; 16] {
    [b; 16]
}

// ---- (c) OutstandingSlot -------------------------------------------

#[test]
fn record_then_recorded_completes() {
    let mut slot = OutstandingSlot::new();
    let k = slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
    assert_eq!(k, key(1));
    assert!(slot.outstanding().is_some());
    let res = slot.apply_outcome(InputWireOutcome::Recorded, 1, || key(2));
    assert_eq!(res, OutstandingResolution::Completed);
    assert!(slot.outstanding().is_none());
}

#[test]
fn delivery_unknown_is_never_auto_retried() {
    let mut slot = OutstandingSlot::new();
    slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
    let res = slot.apply_outcome(InputWireOutcome::DeliveryUnknown, 1, || key(2));
    assert_eq!(res, OutstandingResolution::Unknown);
    assert!(slot.outstanding().is_none());
}

#[test]
fn refused_stale_mints_a_new_key_under_the_new_epoch() {
    let mut slot = OutstandingSlot::new();
    slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
    let res = slot.apply_outcome(InputWireOutcome::RefusedStale, 7, || key(2));
    assert_eq!(res, OutstandingResolution::RetryNewEpoch { idem_key: key(2) });
    let o = slot.outstanding().unwrap();
    assert_eq!(o.idem_key, key(2));
    assert_eq!(o.take_epoch, 7);
}

#[test]
fn reconnect_within_the_same_voyage_resends_the_same_key() {
    let mut slot = OutstandingSlot::new();
    slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
    let decision = slot.resend_after_reconnect("v1", 9);
    assert_eq!(decision, ReconnectResendDecision::Resend { idem_key: key(1) });
    assert_eq!(slot.outstanding().unwrap().take_epoch, 9);
    assert_eq!(slot.outstanding().unwrap().idem_key, key(1));
}

#[test]
fn reconnect_across_a_voyage_change_cancels_and_reports_the_canceled_tuple() {
    let mut slot = OutstandingSlot::new();
    slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
    let decision = slot.resend_after_reconnect("v2", 1);
    match decision {
        ReconnectResendDecision::Cancel { canceled } => {
            assert_eq!(canceled.voyage_uuid, "v1");
            assert_eq!(canceled.bytes, b"hi");
        }
        other => panic!("expected Cancel, got {other:?}"),
    }
    assert!(slot.outstanding().is_none());
}

#[test]
fn quit_with_outstanding_reports_it_rather_than_dropping_silently() {
    let mut slot = OutstandingSlot::new();
    slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
    let reported = slot.cancel_for_quit();
    assert_eq!(reported.unwrap().bytes, b"hi");
    assert!(slot.outstanding().is_none());
}

#[test]
fn apply_outcome_with_nothing_outstanding_is_a_harmless_no_op() {
    let mut slot = OutstandingSlot::new();
    let res = slot.apply_outcome(InputWireOutcome::Recorded, 1, || key(1));
    assert_eq!(res, OutstandingResolution::NothingOutstanding);
}
