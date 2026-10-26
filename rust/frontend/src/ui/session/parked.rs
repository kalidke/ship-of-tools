//! The rows' parked views, keyed by row.

use super::*;

/// The rows' parked views: one `WorkspaceUiSnapshot` per row the window has left, keyed by its `WsKey`. Leaving a
/// row parks its view, coming back reads it, and a row's own end edits or drops it; nothing else reaches the map.
#[derive(Default)]
pub(in crate::ui) struct ParkedViews {
    views: HashMap<WsKey, WorkspaceUiSnapshot>,
}

impl ParkedViews {
    /// Park a left row's view, replacing any earlier one.
    pub(in crate::ui) fn park(&mut self, key: WsKey, view: WorkspaceUiSnapshot) {
        self.views.insert(key, view);
    }

    pub(in crate::ui) fn view(&self, key: &WsKey) -> Option<&WorkspaceUiSnapshot> {
        self.views.get(key)
    }

    /// A kept default row's run ended: its parked view no longer names an attachment to restore.
    pub(in crate::ui) fn forget_attachment(&mut self, key: &WsKey) {
        if let Some(view) = self.views.get_mut(key) {
            view.forget_attachment();
        }
    }

    /// A destroyed row's view dies with it, so a same-slug successor starts clean.
    pub(in crate::ui) fn forget(&mut self, key: &WsKey) {
        self.views.remove(key);
    }

    /// Move a view parked under the default row's raw slug to its `<default>` key; a view already there wins.
    pub(in crate::ui) fn migrate(&mut self, from: &WsKey, to: WsKey) {
        if let Some(view) = self.views.remove(from) {
            self.views.entry(to).or_insert(view);
        }
    }
}
