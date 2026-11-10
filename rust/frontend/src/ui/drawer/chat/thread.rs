//! The one open thread. It keeps its own copy of the parent, so the thread outlives the channel's record, and the channel's
//! view, so the thread has a view of its own.

use sot_protocol::chat::ChatRecord;

use super::view::ChatView;

pub(super) struct Thread {
    parent: ChatRecord,
    /// The parent was deleted when the thread opened.
    deleted: bool,
    /// The channel's view, with its position and mark, until the thread closes.
    pub parked: ChatView,
}

impl Thread {
    pub fn new(parent: ChatRecord, deleted: bool, parked: ChatView) -> Self {
        Thread { parent, deleted, parked }
    }

    pub fn records(&self) -> Vec<(&ChatRecord, bool)> {
        vec![(&self.parent, self.deleted)]
    }
}
