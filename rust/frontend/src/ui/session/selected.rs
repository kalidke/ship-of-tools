//! The selected row: the (host, workspace) every current-view operation targets, its read-mark dwell, the
//! two-press destroy confirm and a create's request time.

use super::presence::{read_mark_decision, ReadMarkAction};
use super::{ReadMark, READ_DWELL};
use crate::net::dial::HostKey;
use crate::ui::WsKey;

/// The pair `(host, workspace_id)` names the current row (ADR 0042 L2a): the host is the connection every
/// current-view send routes to, and `None` for the workspace is that daemon's default. Only a switch (or the
/// capture harness at startup) selects another row; a person's switch also arms the row's read mark.
pub(in crate::ui) struct SelectedRow {
    host: HostKey,
    workspace_id: Option<String>,
    /// Pending 10 s read mark for the row a person just switched to.
    read_mark: Option<ReadMark>,
    /// The row a first destroy press armed; any next key takes it, and only a second press on that row destroys.
    destroy_armed: Option<WsKey>,
    /// When `workspace.create` was sent, so the attach the create's switch makes counts from the request.
    create_requested_at: Option<std::time::Instant>,
}

impl SelectedRow {
    pub(in crate::ui) fn new(host: HostKey, workspace_id: Option<String>) -> Self {
        Self { host, workspace_id, read_mark: None, destroy_armed: None, create_requested_at: None }
    }

    pub(in crate::ui) fn host(&self) -> &HostKey { &self.host }
    pub(in crate::ui) fn workspace_id(&self) -> &Option<String> { &self.workspace_id }

    /// Select another row. The read mark is untouched: the dwell drops a mark whose row is no longer selected.
    pub(in crate::ui) fn select(&mut self, host: HostKey, workspace_id: Option<String>) {
        self.host = host;
        self.workspace_id = workspace_id;
    }

    /// Every switch replaces the mark: a person's switch arms one for the selected row, due `READ_DWELL` from `now`;
    /// any other switch leaves none.
    pub(in crate::ui) fn arm_read_mark(&mut self, person_driven: bool, now: std::time::Instant) {
        self.read_mark = person_driven.then(|| ReadMark {
            host: self.host.clone(),
            workspace_id: self.workspace_id.clone(),
            at: now + READ_DWELL,
        });
    }

    /// The mark that is due at `now` with its row still selected, taken; a mark whose row was left is dropped.
    pub(in crate::ui) fn take_due_read_mark(&mut self, now: std::time::Instant) -> Option<ReadMark> {
        match read_mark_decision(self.read_mark.as_ref(), &self.host, self.workspace_id.as_deref(), now) {
            ReadMarkAction::Keep => None,
            ReadMarkAction::Cancel => {
                self.read_mark = None;
                None
            }
            ReadMarkAction::Fire => self.read_mark.take(),
        }
    }

    pub(in crate::ui) fn arm_destroy(&mut self, target: WsKey) {
        self.destroy_armed = Some(target);
    }

    /// Every key press takes the arm first, so anything but a second destroy press on the same row disarms it.
    pub(in crate::ui) fn take_destroy_arm(&mut self) -> Option<WsKey> {
        self.destroy_armed.take()
    }

    pub(in crate::ui) fn note_create_requested(&mut self, at: std::time::Instant) {
        self.create_requested_at = Some(at);
    }

    /// Taken by the next attach, so a stale time never reaches a later, unrelated switch.
    pub(in crate::ui) fn take_create_requested(&mut self) -> Option<std::time::Instant> {
        self.create_requested_at.take()
    }
}
