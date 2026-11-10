//! The one open thread. It keeps its own copy of the parent, so the thread outlives the channel's record.

use sot_protocol::chat::ChatRecord;

pub(super) struct Thread {
    parent: ChatRecord,
}

impl Thread {
    pub fn new(parent: ChatRecord) -> Self {
        Thread { parent }
    }

    /// The parent, none deleted.
    pub fn records(&self) -> Vec<(&ChatRecord, bool)> {
        vec![(&self.parent, false)]
    }
}
