// the window lease (fe.lease, fe.leaving, fe.notice_seen) and the close lifecycle's bounds and exit codes

use super::*;

/// `fe.lease` request: a window claims the daemon. `boot`, `pid` and
/// `created` identify the claimant's process; on Windows `boot` is the
/// registry BootId as a decimal string, `""` if unreadable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeLeaseReq {
    pub boot: String,
    pub pid: u32,
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// How the daemon answered a lease claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseOutcome {
    Granted,
    Foreign,
    Undetermined,
    Closing,
}

/// `fe.lease` reply. `state_root` is set only when `Granted`; `not_ended`
/// counts sessions an earlier close could not end (0 = nothing to show).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeLeaseRes {
    pub outcome: LeaseOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_root: Option<String>,
    #[serde(default)]
    pub not_ended: u32,
}

/// What the leaving window wants done with the computer's sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaveIntent {
    Close,
    Keep,
    Handover,
}

/// `fe.leaving` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeLeavingReq {
    pub intent: LeaveIntent,
}

/// `fe.leaving` reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeLeavingRes {
    #[serde(default)]
    pub not_ended: u32,
}

/// `fe.notice_seen` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeNoticeSeenReq {
    pub not_ended: u32,
}

/// `fe.notice_seen` reply — empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeNoticeSeenRes {}

// The close lifecycle's bounds and exit codes, shared by daemon, window and launchers.
use std::time::Duration;
pub const HANDOVER_BOUND: Duration = Duration::from_secs(60);
/// Decision to process exit.
pub const SHUTDOWN_BOUND: Duration = Duration::from_secs(120);
/// Reserved for the last steps of a shutdown (4-6).
pub const SHUTDOWN_TAIL: Duration = Duration::from_secs(10);
pub const DAEMON_LOCK_WAIT: Duration = Duration::from_secs(150);
pub const LAUNCH_WAIT: Duration = Duration::from_secs(160);
pub const LEASE_REPLY_WAIT: Duration = Duration::from_secs(5);
/// Keep and Handover acks.
pub const KEEP_ACK_WAIT: Duration = Duration::from_secs(10);
/// `SHUTDOWN_BOUND` + 5 s.
pub const CLOSE_ACK_WAIT: Duration = Duration::from_secs(125);
pub const NOTICE_ACK_WAIT: Duration = Duration::from_secs(5);
pub const EXIT_REQUESTED_SHUTDOWN: i32 = 0;
pub const EXIT_UPDATE_RESTART: i32 = 75;
/// In the daemon's state root.
pub const HELD_RECORD_FILE: &str = "held.json";

#[cfg(test)]
mod lease_wire_tests {
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
}
