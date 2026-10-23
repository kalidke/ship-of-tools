// lane.connect, the daemon-bridged capsule lane

use super::*;

/// `lane.connect` request (ADR 0045 §2, protocol 4) — the frame behind a `handoff` hello on a dedicated
/// lane-bridge connection. `target` is the capsule row's `session_name` name, exactly as `pty.open` addresses it; the
/// daemon dials that row's one socket, `row-<h>`. `token` mirrors `ProxyConnectReq::token`.
///
/// Error codes on refusal (standard error payload, connection closes):
/// `bad_request` (payload didn't parse), `unauthenticated` (no daemon
/// in this tree sends it; only a daemon from before the token was
/// removed in 0.4.0 answers `lane.connect` with it),
/// `unknown_workspace` (`target` names no row), `lane_absent` (the
/// row socket is not there right now, after a resume attempt was tried
/// or refused; payload also carries `"kind": "<io ErrorKind
/// Debug>"`), `foreign` (the peer behind the socket failed identity
/// authentication), `undetermined` (identity authentication could not
/// be completed), `dial_failed` (any other I/O error dialing or authenticating).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneConnectReq {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// `lane.connect` response. After `{ok: true, pid, created}` the
/// connection stops being a frame stream: every subsequent byte in BOTH
/// directions is piped verbatim to/from the dialed lane, exactly as
/// `proxy.connect` pipes to a loopback port. `pid`/`created` are the
/// lane peer's identity, as the daemon's own dial observed it
/// (`PeerAuthenticated` — steps 1-3 of the challenge) — the frontend
/// trusts the daemon it already trusts to control the row, and binds its
/// own end-to-end `hello` (steps 4-5) against this report. Errors ride
/// the standard error payload instead (see [`LaneConnectReq`]'s own doc
/// for the codes) and the connection closes without entering pipe mode.
///
/// Limitation (Codex review, 2026-09-11): this report is the daemon's
/// OWN OS-level identity observation of whatever it dialed, not a
/// credential — the frontend cannot independently verify it, and this
/// reply retains no process handle of its own (no `reverify`/`wait`/
/// `terminate` capability travels with it, unlike the full five-step
/// [`crate::challenge::ChallengeOutcome::Proven`] proof `PeerProcess`
/// grants a step-6 supervisor). The frontend's own trust here is
/// entirely delegated: it trusts the daemon it already trusts to
/// control the row, nothing more.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneConnectRes {
    pub ok: bool,
    pub pid: u32,
    pub created: u64,
}
