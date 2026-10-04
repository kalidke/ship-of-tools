//! The hello: its reply bound, its refusal and the protocol-mismatch message.

use super::*;

/// How long the hello reply may take. An ssh child stalled before auth
/// leaves the pipe open and silent, so without a bound the one control
/// transport waits forever; on expiry the read errors like an EOF and the
/// reconnect loop backs off normally, dropping the child.
pub(super) const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Read the hello reply frame, bounded by `timeout` (a parameter so a test
/// can pass a short one).
pub(super) async fn read_hello_reply<R: tokio::io::AsyncBufRead + Unpin>(
    rx: &mut R,
    timeout: std::time::Duration,
) -> Result<(Frame, Option<Vec<u8>>)> {
    tokio::time::timeout(timeout, codec::read_frame(rx))
        .await
        .map_err(|_| anyhow::anyhow!("hello reply timed out after {timeout:?}"))?
}

/// The daemon answered the hello with a refusal (bad token, protocol skew):
/// a reply, so the link itself is fine and the gate stays up.
#[derive(Debug)]
pub(super) struct HelloRefused(pub(super) String);

impl std::fmt::Display for HelloRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HelloRefused {}

/// The blocking "update needed" body for a `protocol_mismatch` hello
/// refusal (ADR 0030 §2), naming BOTH sides from the daemon's structured
/// payload and this build's own constants — so an old daemon and a new
/// frontend fail as loudly as the reverse skew, which the daemon's own
/// hello gate names (`sot-backend`'s `handle_hello`).
pub(crate) fn protocol_mismatch_message(payload: &serde_json::Value, err_msg: &str) -> String {
    let get_str = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let get_u32 = |k: &str| payload.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let backend_version = {
        let v = get_str("backend_version");
        if v.is_empty() {
            "<unknown>".to_string()
        } else {
            v.to_string()
        }
    };
    let theirs = get_u32("backend_protocol");
    let ours = u64::from(sot_protocol::PROTOCOL_VERSION);
    let behind = if theirs > ours { "frontend" } else { "daemon" };
    format!(
        "{behind} out of date — daemon {backend_version} speaks protocol {theirs}, this frontend {} speaks {ours}\n\n\
         backend:  {}  (protocol {})\n\
         frontend: {}  (protocol {})\n\n\
         dev: git pull + rebuild + relaunch · see docs/adr/0030\n\n\
         ({err_msg})",
        sot_protocol::app_version(),
        backend_version,
        theirs,
        sot_protocol::app_version(),
        sot_protocol::PROTOCOL_VERSION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hello_read_times_out_against_a_silent_peer() {
        // The far end is held open and never written: without a bound the
        // read waits forever.
        let (_far, near) = tokio::io::duplex(64);
        let mut rx = tokio::io::BufReader::new(near);
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_hello_reply(&mut rx, std::time::Duration::from_millis(100)),
        )
        .await
        .expect("hang guard: the bounded read must return");
        let err = r.expect_err("a silent peer must time out");
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[test]
    fn protocol_mismatch_message_names_both_sides_of_the_skew() {
        // A NEW frontend against an OLD daemon: the daemon's gate answers
        // with its own version; this build adds its own. Both must appear.
        let payload = serde_json::json!({
            "code": "protocol_mismatch",
            "backend_protocol": sot_protocol::PROTOCOL_VERSION - 1,
            "backend_version": "0.5.9",
        });
        let msg = super::protocol_mismatch_message(&payload, "protocol mismatch");
        assert!(msg.contains(&format!("backend:  0.5.9  (protocol {})", sot_protocol::PROTOCOL_VERSION - 1)), "{msg}");
        assert!(
            msg.contains(&format!("frontend: {}  (protocol {})", sot_protocol::app_version(), sot_protocol::PROTOCOL_VERSION)),
            "{msg}"
        );
    }

    #[test]
    fn protocol_mismatch_headline_names_the_side_that_is_behind() {
        let headline = |theirs: u32, version: &str| {
            let payload = serde_json::json!({
                "code": "protocol_mismatch",
                "backend_protocol": theirs,
                "backend_version": version,
            });
            let msg = super::protocol_mismatch_message(&payload, "protocol mismatch");
            msg.lines().next().unwrap().to_string()
        };
        let ahead = sot_protocol::PROTOCOL_VERSION + 1;
        let first = headline(ahead, "9.9.9");
        assert!(first.starts_with("frontend out of date"), "{first}");
        assert!(first.contains(&format!("protocol {ahead}")), "{first}");
        let first = headline(sot_protocol::PROTOCOL_VERSION - 1, "0.5.9");
        assert!(first.starts_with("daemon out of date"), "{first}");
    }
}
