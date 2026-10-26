//! What the window shows of its connections: the active connection's hello facts for the status line and the
//! version stamp, and each host's refused hello.

use std::collections::HashMap;

use crate::net::dial::HostKey;

/// The active connection's hello, as the status line and the version stamp show it. ADR 0042 L2a: updated only from
/// the active host's own `Connected` (or relabelled by a switch); a non-active host's hello never reaches it.
pub(in crate::ui) struct ActiveLink {
    /// Short hostname from the hello response, kept alongside the
    /// daemon's project_root basename so the chrome can rebuild the
    /// "connected · host:workspace · rev N" status line every time the
    /// active workspace changes — not just at connect time.
    host_label: Option<String>,
    /// Project_root basename from the hello response. Used as the label
    /// whenever the selected workspace is the default; explicit slugs use presentation labels.
    root_basename: Option<String>,
    /// Full project_root path from the hello response. Joined with the
    /// `files:<rel>` node id by `copy_navtree_path` so Ctrl+C in NavTree
    /// yields the absolute backend-side path (paste-into-shell utility).
    project_root: Option<String>,
    /// The connected backend's product version (`HelloRes::app_version`),
    /// painted beside the FE's own version on the bottom chrome edge.
    /// `None` before the first hello, or when the daemon is old enough not
    /// to send the field. Deliberately NOT cleared on disconnect — a stale
    /// "what we last talked to" reads better than a blank half, and the
    /// transport state is already called out in the nav status row, so the
    /// stamp doesn't need to duplicate it.
    backend_version: Option<String>,
    /// Highest revision seen in any frame, surfaced in the status line.
    /// Tracked separately so post-connect events (workspace switches,
    /// preview.changed bumps) don't render a stale rev.
    revision: u64,
}

impl ActiveLink {
    pub(in crate::ui) fn new() -> Self {
        Self { host_label: None, root_basename: None, project_root: None, backend_version: None, revision: 0 }
    }

    pub(in crate::ui) fn host_label(&self) -> Option<&str> { self.host_label.as_deref() }
    pub(in crate::ui) fn root_basename(&self) -> Option<&str> { self.root_basename.as_deref() }
    pub(in crate::ui) fn project_root(&self) -> Option<&str> { self.project_root.as_deref() }
    pub(in crate::ui) fn backend_version(&self) -> Option<&str> { self.backend_version.as_deref() }
    pub(in crate::ui) fn revision(&self) -> u64 { self.revision }

    /// The active host's `Connected`: its label, project root, product version (an empty one, from a
    /// pre-versioning daemon, is kept as `None` so the stamp renders `be ?`) and revision.
    pub(in crate::ui) fn connected(
        &mut self, label: String, project_root: Option<String>, backend_version: String, revision: u64,
    ) {
        self.host_label = Some(label);
        self.root_basename = project_root.as_deref().and_then(|p| {
            p.rsplit(['/', '\\'])
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
        self.project_root = project_root;
        self.backend_version = Some(backend_version).filter(|v| !v.is_empty());
        self.revision = revision;
    }

    /// A switch to a host that is already connected fires no new `Connected`, so the switch relabels.
    pub(in crate::ui) fn relabel(&mut self, label: String) { self.host_label = Some(label); }
}

/// ADR 0030 §2 / ADR 0042 L2a: a persistent, blocking "update needed"
/// message shown when a host's daemon refuses the handshake on a
/// protocol-version skew (ADR 0049: or this window's account). Keyed by host — each host's own hello
/// answers for itself — so ONE stale/optional remote can't block the
/// whole UI while every other (healthy) host works fine; only
/// the active host's entry is projected to the blocking overlay. A host's entry is removed on
/// its own next successful `Connected` (not every host's).
#[derive(Default)]
pub(in crate::ui) struct HelloRefusals {
    by_host: HashMap<HostKey, String>,
}

impl HelloRefusals {
    pub(in crate::ui) fn get(&self, host: &HostKey) -> Option<&str> { self.by_host.get(host).map(String::as_str) }
    pub(in crate::ui) fn refused(&self, host: &HostKey) -> bool { self.by_host.contains_key(host) }
    pub(in crate::ui) fn refuse(&mut self, host: HostKey, message: String) { self.by_host.insert(host, message); }
    /// The host's own clean hello: its refusal, and only its, is lifted.
    pub(in crate::ui) fn accept(&mut self, host: &HostKey) { self.by_host.remove(host); }
}
