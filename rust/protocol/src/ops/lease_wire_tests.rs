use super::*;
use crate::Frame;

#[test]
fn bounds_chain() {
    use lease::*;
    assert!(HANDOVER_BOUND < SHUTDOWN_BOUND);
    assert!(SHUTDOWN_BOUND < DAEMON_LOCK_WAIT);
    assert!(DAEMON_LOCK_WAIT < LAUNCH_WAIT);
    assert!(SHUTDOWN_TAIL < SHUTDOWN_BOUND);
    assert_eq!(CLOSE_ACK_WAIT, SHUTDOWN_BOUND + std::time::Duration::from_secs(5));
}

#[test]
fn fe_lease_golden_line() {
    let req = FeLeaseReq { boot: String::new(), pid: 4242, created: 133_000_000_000_000_000, token: None };
    let f = Frame::req(1, op::FE_LEASE, serde_json::to_value(&req).unwrap());
    assert_eq!(
        serde_json::to_string(&f).unwrap(),
        r#"{"v":3,"id":1,"kind":"req","op":"fe.lease","payload":{"boot":"","created":133000000000000000,"pid":4242}}"#
    );
}

#[test]
fn lease_outcome_and_intent_are_snake_case() {
    assert_eq!(serde_json::to_string(&LeaseOutcome::Undetermined).unwrap(), r#""undetermined""#);
    assert_eq!(serde_json::to_string(&LeaseOutcome::Granted).unwrap(), r#""granted""#);
    assert_eq!(serde_json::to_string(&LeaveIntent::Handover).unwrap(), r#""handover""#);
    assert_eq!(serde_json::to_string(&LeaveIntent::Close).unwrap(), r#""close""#);
}

#[test]
fn fe_lease_res_defaults() {
    let r: FeLeaseRes = serde_json::from_str(r#"{"outcome":"granted"}"#).unwrap();
    assert_eq!(r.outcome, LeaseOutcome::Granted);
    assert_eq!(r.state_root, None);
    assert_eq!(r.not_ended, 0);
}

#[test]
fn fe_leaving_res_default_parses_empty_object() {
    let r: FeLeavingRes = serde_json::from_str("{}").unwrap();
    assert_eq!(r.not_ended, 0);
}
