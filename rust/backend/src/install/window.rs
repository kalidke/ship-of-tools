use std::time::Duration;

/// Only the local daemon's connected windows can leave a restart step; silence says nothing.
pub(super) fn restart_step(endpoint: &str, host: &str, installed: &str) -> Option<String> {
    let reply = crate::topology::dial::dial_and_call(endpoint, host, sot_protocol::op::VERSION_QUERY, serde_json::json!({}), Duration::from_secs(3)).ok()?;
    let reply: sot_protocol::ops::VersionQueryRes = serde_json::from_value(reply).ok()?;
    let mut versions = reply.clients.iter()
        .filter(|c| c.role == "fe" && c.host.as_deref() == Some(host) && !c.app_version.is_empty() && c.app_version != installed)
        .map(|c| c.app_version.as_str()).collect::<Vec<_>>();
    versions.sort_unstable();
    versions.dedup();
    (!versions.is_empty()).then(|| format!("quit Ship of Tools and start it again: its connected window on this computer runs {}, installed version {installed}", versions.join(", ")))
}

#[cfg(all(test, unix))]
#[path = "window_tests.rs"]
mod tests;
