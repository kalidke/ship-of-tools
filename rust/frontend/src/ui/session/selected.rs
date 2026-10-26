//! The selected row: the (host, workspace) every current-view operation targets, and its read-mark dwell.

use super::presence::{read_mark_decision, ReadMarkAction};
use super::{ReadMark, READ_DWELL};
use crate::net::dial::HostKey;

/// The pair `(host, workspace_id)` names the current row (ADR 0042 L2a): the host is the connection every
/// current-view send routes to, and `None` for the workspace is that daemon's default. Only a switch (or the
/// capture harness at startup) selects another row; a person's switch also arms the row's read mark.
pub(in crate::ui) struct SelectedRow {
    host: HostKey,
    workspace_id: Option<String>,
    /// Pending 10 s read mark for the row a person just switched to.
    read_mark: Option<ReadMark>,
}

impl SelectedRow {
    pub(in crate::ui) fn new(host: HostKey, workspace_id: Option<String>) -> Self {
        Self { host, workspace_id, read_mark: None }
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
}
