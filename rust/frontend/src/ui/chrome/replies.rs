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
        if event_host == self.active_host {
            self.host = Some(host_label(&self.hosts.declared_host, &event_host).to_string());
            self.daemon_root_basename = project_root.as_deref().and_then(|p| {
                p.rsplit(['/', '\\'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            });
            self.daemon_project_root = project_root.clone();
            // Backend product version for the bottom-edge version
            // stamp. Empty (pre-versioning daemon) is kept as `None`
            // so the stamp renders `be ?` rather than a blank half.
            self.backend_version = Some(backend_version).filter(|v| !v.is_empty());
            self.last_revision = revision;
        }
    }
}
