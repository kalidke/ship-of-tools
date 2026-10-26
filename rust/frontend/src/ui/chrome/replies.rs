//! The status-line fields a host's Connected event sets when it is the active host: host label,
//! daemon root, backend version, revision.

use crate::ui::*;

impl State {
    pub(crate) fn set_status_line_fields(
        &mut self,
        event_host: HostKey,
        revision: u64,
        project_root: Option<String>,
        backend_version: String,
    ) {
        // Cache host + daemon root basename so the chrome can
        // rebuild the connection status every time the active
        // workspace changes — not just at hello time. Manager
        // review (S9, finding S14): no separate truncation
        // here — `host_label` (already updated above for
        // `event_host`, from this same `Connected` event) is
        // the ONE display projection every host-keyed surface
        // uses.
        //
        // ADR 0042 L2a: these four fields describe the ACTIVE
        // connection's status line, not every connection — a
        // Connected from a non-active host still flips
        // `host_connected` above (so its tree node updates) but
        // must not overwrite what the status line shows for the
        // host the user is actually looking at.
        if event_host == *self.selected.host() {
            let label = host_label(&self.hosts.declared_host, &event_host).to_string();
            self.active_link.connected(label, project_root, backend_version, revision);
        }
    }
}
