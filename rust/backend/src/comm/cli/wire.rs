//! The two daemons a command may ask, and the two requests it makes: `comm.file` (a line this box cannot append
//! itself) and `agent.join` (a row's declaration, or a ruling for a join from outside any row). Every connection is
//! `topology::dial`'s: only a daemon of this OS account, hello first, and an answer within the bound or none.

use std::time::Duration;

/// This box's own daemon's endpoint when one is there to ask: a socket that exists (Unix), a pipe this account
/// serves (Windows). The shell's choice of "the daemon that owns this folder" starts here.
pub(crate) fn own_daemon(env: &crate::comm::Env) -> Option<String> {
    let ep = sot_topology::endpoint::local_endpoint_in(env);
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
        sot_platform::transport::connect_own::connect_own(std::path::Path::new(path)).ok()?;
        Some(ep)
    }
}

/// The relay endpoint for this box, `sotd topology relay-endpoint`'s answer: the hub, or on the hub its own daemon;
/// this box's own endpoint where no topology was ever declared. `Err` is the topology's own words.
pub(crate) fn relay_endpoint(env: &crate::comm::Env) -> Result<String, String> {
    let me = sot_platform::host::state_dir::host_name_in(env)?;
    match sot_topology::load_in(env)? {
        Some((_, t)) => sot_topology::relay_endpoint(&t, &me),
        None => Ok(sot_topology::endpoint::local_endpoint_in(env)),
    }
}

/// How long a sender waits for a filing's answer: decision 0031's 5 s, whatever the inbox lock's wait.
pub(crate) const FILE_WINDOW: Duration = Duration::from_secs(5);

/// One `comm.file` for `to` at `endpoint`. `Ok` only on the daemon's `ok`; `Err` is the sentence after `FAILED ->
/// @to: `: the daemon's own refusal, a refused hello, or that nothing answered.
pub(crate) fn comm_file(env: &crate::comm::Env, endpoint: &str, from: &str, to: &str, text: &str, broadcast: bool) -> Result<(), String> {
    let req = sot_protocol::CommFileReq { from: from.into(), to: to.into(), text: text.into(), broadcast, forwarded: false };
    let payload = serde_json::to_value(req).map_err(|e| e.to_string())?;
    let answer = call(env, endpoint, sot_protocol::op::COMM_FILE, payload, FILE_WINDOW)?;
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
pub(crate) fn agent_join(env: &crate::comm::Env, endpoint: &str, workspace_id: &str, handle: &str) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({ "workspace_id": workspace_id, "handle": handle });
    call(env, endpoint, sot_protocol::op::AGENT_JOIN, payload, Duration::from_secs(10))
}

/// How long `comm-list.sh` waits for the daemon's roster: one op on this computer, answered in milliseconds.
pub(crate) const LIST_WINDOW: Duration = Duration::from_secs(5);

/// One `version.query` at `endpoint`: the daemon's version and the frontends attached to it, or why there is none.
pub(crate) fn version_query(env: &crate::comm::Env, endpoint: &str) -> Result<sot_protocol::VersionQueryRes, String> {
    let answer = call(env, endpoint, sot_protocol::op::VERSION_QUERY, serde_json::json!({}), LIST_WINDOW)?;
    if let Some(e) = answer.get("error") {
        return Err(format!("the daemon at {endpoint} refused version.query: {}", e.as_str().map_or_else(|| e.to_string(), str::to_string)));
    }
    serde_json::from_value(answer).map_err(|e| format!("the daemon at {endpoint} answered version.query in another shape: {e}"))
}

/// One request and its answer; `Err` names a refused hello in its own words, else that nothing answered.
fn call(env: &crate::comm::Env, endpoint: &str, op: &str, payload: serde_json::Value, within: Duration) -> Result<serde_json::Value, String> {
    let me = sot_platform::host::state_dir::host_name_in(env).unwrap_or_default();
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
