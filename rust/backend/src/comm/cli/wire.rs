//! The two daemons a command may ask, and the two requests it makes: `comm.file` (a line this box cannot append
//! itself) and `agent.join` (a row's declaration, or a ruling for a join from outside any row). Every connection is
//! `topology::dial`'s: only a daemon of this OS account, hello first, and an answer within the bound or none.

use std::time::Duration;

/// This box's own daemon's endpoint when one is there to ask: a socket that exists (Unix), a pipe this account
/// serves (Windows). The shell's choice of "the daemon that owns this folder" starts here.
pub(crate) fn own_daemon() -> Option<String> {
    let ep = sot_protocol::topology::endpoint::local_endpoint();
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let path = ep.strip_prefix("unix:")?;
        std::fs::metadata(path).ok().filter(|m| m.file_type().is_socket())?;
        Some(ep)
    }
    #[cfg(windows)]
    {
        let path = ep.strip_prefix("pipe:")?;
        sot_log::identity::connect_own::connect_own(std::path::Path::new(path)).ok()?;
        Some(ep)
    }
}

/// The relay endpoint for this box, `sotd topology relay-endpoint`'s answer: the hub, or on the hub its own daemon;
/// this box's own endpoint where no topology was ever declared. `Err` is the topology's own words.
pub(crate) fn relay_endpoint() -> Result<String, String> {
    let me = sot_log::host::state_dir::host_name()?;
    match sot_protocol::topology::load()? {
        Some((_, t)) => sot_protocol::topology::relay_endpoint(&t, &me),
        None => Ok(sot_protocol::topology::endpoint::local_endpoint()),
    }
}

/// How long a sender waits for a filing's answer: the inbox lock's wait plus 3 s (decision 0031's 5 s at the default),
/// as an appender that waited out the lock and then filed is not a silent one.
pub(crate) fn file_window() -> Duration {
    crate::comm::mail::inbox::inbox_lock_wait() + Duration::from_secs(3)
}

/// One `comm.file` for `to` at `endpoint`. `Ok` only on the daemon's `ok`; `Err` is the sentence after `FAILED ->
/// @to: `: the daemon's own refusal, a refused hello, or that nothing answered.
pub(crate) fn comm_file(endpoint: &str, from: &str, to: &str, text: &str, broadcast: bool) -> Result<(), String> {
    let req = sot_protocol::CommFileReq { from: from.into(), to: to.into(), text: text.into(), broadcast, forwarded: false };
    let payload = serde_json::to_value(req).map_err(|e| e.to_string())?;
    let answer = call(endpoint, sot_protocol::op::COMM_FILE, payload, file_window())?;
    if answer.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }
    match answer.get("error") {
        Some(serde_json::Value::String(e)) => Err(e.clone()),
        Some(e) => Err(e.to_string()),
        None => Err(format!("the daemon at {endpoint} answered neither ok nor a refusal: {answer}")),
    }
}

/// One `agent.join` at `endpoint`: the answer's payload, or why there is none.
pub(crate) fn agent_join(endpoint: &str, workspace_id: &str, handle: &str) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({ "workspace_id": workspace_id, "handle": handle });
    call(endpoint, sot_protocol::op::AGENT_JOIN, payload, Duration::from_secs(10))
}

/// One request and its answer; `Err` names a refused hello in its own words, else that nothing answered.
fn call(endpoint: &str, op: &str, payload: serde_json::Value, within: Duration) -> Result<serde_json::Value, String> {
    let me = sot_log::host::state_dir::host_name().unwrap_or_default();
    crate::topology::dial::dial_and_call(endpoint, &me, op, payload, within).map_err(|e| said(endpoint, &e))
}

/// The sentence a failed dial gives the sender: a refused hello in the daemon's own words, else that nothing answered.
fn said(endpoint: &str, e: &str) -> String {
    let e = e.strip_prefix(&format!("{endpoint}: ")).unwrap_or(e);
    match e.split_once("hello refused: ") {
        Some((_, why)) => format!("hello refused: {why}"),
        None => format!("the daemon did not answer at {endpoint}: {e}"),
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
