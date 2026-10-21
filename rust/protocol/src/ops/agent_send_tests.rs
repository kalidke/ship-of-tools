use super::{AgentSendReq, AgentSendRes};

#[test]
fn a_notification_and_its_answer_carry_nothing_of_the_receipt_route() {
    // A sender of the retired receipt route still parses; its `id`, and an old answer's `receivers` and `id`,
    // are dropped and never written again.
    let req: AgentSendReq =
        serde_json::from_str(r#"{"from":"a","to":"","text":"hi","id":"x-1"}"#).expect("parses, extra keys ignored");
    assert_eq!(serde_json::to_string(&req).unwrap(), r#"{"from":"a","to":"","text":"hi"}"#);
    let res: AgentSendRes =
        serde_json::from_str(r#"{"ok":true,"receivers":["fe@b"],"id":"x-1"}"#).expect("parses, extra keys ignored");
    assert_eq!(serde_json::to_string(&res).unwrap(), r#"{"ok":true}"#);
}
