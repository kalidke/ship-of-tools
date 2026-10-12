// agent messaging: agent.send (UI notifications), comm.file (mail), agent.join

use super::*;

/// `agent.send` request — one UI notification (the nav envelope, the ROI echo), which the daemon stamps with a `ts`
/// and publishes as an `agent.message` evt to every connection. `to` must be empty: a request that names a handle is
/// mail, and mail uses `comm.file`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSendReq {
    pub from: String,
    pub to: String,
    pub text: String,
}

/// `agent.send` response: the daemon published the notification. It says nothing about who saw it, and it is not a
/// filing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSendRes {
    pub ok: bool,
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
    /// Set by a guest daemon that forwards this request to its folder's hub, and by the hub on the request it offers
    /// a linked daemon. A frame takes at most this one hop, so no daemon forwards or offers it again.
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
mod agent_send_tests {
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
}
