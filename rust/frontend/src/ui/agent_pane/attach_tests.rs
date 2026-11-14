use super::*;

#[test]
fn a_key_whose_delivery_is_unknown_is_said_so() {
    use sot_capsule::attach_client::client::InputOutcome;
    assert_eq!(input_outcome_notice(None), None);
    assert_eq!(input_outcome_notice(Some(InputOutcome::Recorded)), None);
    let unknown = input_outcome_notice(Some(InputOutcome::DeliveryUnknown)).expect("unknown delivery has words");
    assert!(unknown.contains("delivery unknown"), "{unknown}");
    assert!(input_outcome_notice(Some(InputOutcome::RunEnded)).is_some_and(|t| t.contains("run ended")));
}

#[test]
fn keys_the_row_refused_are_said_not_sent() {
    use sot_capsule::attach_client::client::InputOutcome;
    let said = input_outcome_notice(Some(InputOutcome::RefusedStale)).expect("a refused key has words");
    assert!(said.contains("not sent"), "{said}");
}
