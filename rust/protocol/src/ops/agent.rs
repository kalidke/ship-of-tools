// agent messaging: agent.send, agent.filed, agent.receipt, comm.file, agent.join

use super::*;

/// `agent.send` request — relay one agent-to-agent message through the
/// daemon. `to == ""` means broadcast to every connection (all machines).
/// The daemon stamps a `ts` and re-emits the body as an `agent.message`
/// evt to every connection (including the sender's, which is harmless —
/// the receiving agent dedups on `ts` if needed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSendReq {
    pub from: String,
    pub to: String,
    pub text: String,
    /// Sender-minted opaque id (ADR 0048), absent from an older sender.
    /// Attributability is the whole invariant: a receipt must be
    /// attributable to exactly the frame it acknowledges, or two
    /// concurrent sends to one handle can swap verdicts and a single
    /// success vouch for a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// `agent.send` response. `receivers` names the connections the daemon's
/// client roster showed as positioned to see `to`, snapshotted BEFORE the
/// publish (`Clients::receivers_for`). A backend's hub link (role `cli`) is
/// not on that roster yet can file the frame, so an empty list does not
/// mean nobody will read it. `ok` reflects
/// only that the request parsed and was published onto the broadcast bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSendRes {
    pub ok: bool,
    pub receivers: Vec<String>,
    /// The `id` this request carried, echoed back (ADR 0048). The ONE
    /// thing that separates "no filer answered" from "this hub cannot
    /// carry an answer": a hub that echoes the id supports receipts, one
    /// that drops it predates them, and a sender can say which without
    /// waiting out a timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// `agent.filed` request (ADR 0048) — "I appended the frame carrying this
/// id". One field, because one field is the whole claim: the handle is the
/// sender's own `to` (it knows what it addressed), and there is no
/// negative form to express (see `AGENT_FILED`). No `filer` field either —
/// the daemon stamps that from the answering connection's own hello
/// `name`, so a filer at least names itself rather than being guessed at.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFiledReq {
    pub id: String,
}

/// `agent.filed` response — a bare ack, mirroring `AgentJoinRes`. The
/// sender's verdict rides the `AGENT_RECEIPT` evt, not this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFiledRes {
    pub ok: bool,
}

/// `agent.receipt` evt (ADR 0048) — one relayed filer claim, with `filer`
/// stamped by the daemon from the answering connection's declared hello
/// `name` rather than from anything the request said. The frame's arrival
/// IS the positive verdict; there is no field to say otherwise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReceiptEvt {
    pub id: String,
    pub filer: String,
}

/// `comm.file` request (0031 B1). `to` addresses the inbox, `from` is stamped
/// into the line, `text` is the message. No id: the verdict is the response
/// to this request on this connection, so there is nothing to correlate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommFileReq {
    pub from: String,
    pub to: String,
    pub text: String,
    /// A broadcast copy never ranks as directed mail: when true the line is
    /// stamped `to:""`, as `comm-send.sh` stamps a `--broadcast` copy, so it
    /// files silently for `comm-poll.sh` instead of waking its reader.
    #[serde(default)]
    pub broadcast: bool,
    /// Set by a guest daemon that forwards this request to its folder's hub.
    /// A frame is forwarded at most once, so a misconfigured hub cannot loop.
    #[serde(default)]
    pub forwarded: bool,
}

/// `comm.file` response: the line is in the file. A refusal is the standard
/// `{error, code}` payload instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommFileRes {
    pub ok: bool,
}

/// `agent.join` request (ADR 0046 decision 1) — a session declares its
/// sot-comm handle to the daemon that owns its workspace. `handle` is
/// validated against the same charset `comm-lib.sh`'s `--name` derivation
/// uses; a request naming a workspace the daemon doesn't have refuses
/// `unknown_workspace`, an invalid `handle` refuses `bad_handle`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentJoinReq {
    pub workspace_id: String,
    pub handle: String,
}

/// `agent.join` response — a simple ack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentJoinRes {
    pub ok: bool,
}

#[cfg(test)]
mod agent_receipt_tests {
    use super::{AgentFiledReq, AgentReceiptEvt, AgentSendReq, AgentSendRes};

    #[test]
    fn agent_send_req_parses_with_and_without_an_id() {
        // Mixed fleet (ADR 0048): an old sender's frame carries no `id`,
        // and must still parse — the daemon then publishes no id, no
        // filer claims anything, and that sender behaves as it always did.
        let old: AgentSendReq =
            serde_json::from_str(r#"{"from":"a","to":"b","text":"hi"}"#).expect("old sender parses");
        assert_eq!(old.id, None);
        let new: AgentSendReq =
            serde_json::from_str(r#"{"from":"a","to":"b","text":"hi","id":"x-1"}"#)
                .expect("new sender parses");
        assert_eq!(new.id.as_deref(), Some("x-1"));
    }

    #[test]
    fn agent_send_res_omits_the_id_key_when_there_is_none() {
        // An old client greps the ack by op and reads `receivers`; a null
        // `id` on the wire would be a new key it has to tolerate, and an
        // ABSENT one is also what tells a new sender "this hub predates
        // receipts". Both readings depend on the key not being emitted.
        let res = AgentSendRes { ok: true, receivers: vec!["fe@otherbox".into()], id: None };
        let j = serde_json::to_string(&res).expect("serializes");
        assert!(!j.contains("\"id\""), "id must be omitted, got {j}");
        let with = AgentSendRes { ok: true, receivers: vec![], id: Some("x-1".into()) };
        assert!(serde_json::to_string(&with).unwrap().contains("\"id\":\"x-1\""));
    }

    #[test]
    fn agent_filed_req_is_one_field_and_expresses_no_negative() {
        // The claim a filer can defend is "I appended this", and that is
        // all the type can say. A sender that tries to express a denial
        // (or to name a filer) is parsed with those keys dropped — no
        // `deny_unknown_fields` anywhere in this lane, which the compat
        // matrix depends on.
        let req: AgentFiledReq = serde_json::from_str(
            r#"{"id":"x-1","handle":"peer","filed":false,"filer":"someone-else"}"#,
        )
        .expect("parses, extra keys ignored");
        assert_eq!(req.id, "x-1");
        let j = serde_json::to_string(&req).expect("serializes");
        assert_eq!(j, r#"{"id":"x-1"}"#, "the claim is one field: {j}");
    }

    #[test]
    fn agent_receipt_evt_round_trips_its_two_fields() {
        let evt = AgentReceiptEvt { id: "x-1".into(), filer: "fe@otherbox".into() };
        let back: AgentReceiptEvt =
            serde_json::from_str(&serde_json::to_string(&evt).unwrap()).expect("round trips");
        assert_eq!(back.id, "x-1");
        assert_eq!(back.filer, "fe@otherbox");
        let j = serde_json::to_string(&evt).unwrap();
        assert!(!j.contains("filed"), "no positive/negative flag on the wire: {j}");
        assert!(!j.contains("reason"), "nothing to give a reason for: {j}");
    }
}
