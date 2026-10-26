//! The reveal walk's state: the target the Files tree is being opened toward, the level it waits on, the one
//! force-refresh it spent, and a target waiting on a root reply.

/// `drive_reveal_step` (reveal.rs) walks the Files tree open one ancestor per `tree.children` round trip; this is all
/// it remembers between round trips. Every transition is named, so each caller states which parts it ends.
#[derive(Default)]
pub(in crate::ui) struct RevealWalk {
    /// Deep-path reveal target (`files:<relpath>`) whose row is not yet visible; `None` when no walk is in flight.
    target: Option<String>,
    /// The ancestor whose `tree.children` the walk requested and awaits; unrelated replies do not advance it.
    awaiting: Option<String>,
    /// `(target, ancestor)` the walk force-refreshed once because an expanded ancestor lacked the target (a file
    /// written after the listing); a second miss means the target is gone.
    refetched: Option<(String, String)>,
    /// A target armed before its row's Files root loaded (a switch, a driven open into an unloaded tree), consumed
    /// by that root's reply.
    root_target: Option<String>,
}

impl RevealWalk {
    pub(in crate::ui) fn target(&self) -> Option<&str> { self.target.as_deref() }
    pub(in crate::ui) fn awaiting(&self) -> Option<&str> { self.awaiting.as_deref() }
    pub(in crate::ui) fn refetched(&self) -> Option<&(String, String)> { self.refetched.as_ref() }
    pub(in crate::ui) fn root_target(&self) -> Option<&str> { self.root_target.as_deref() }

    /// Start a walk toward `target` with fresh per-level bookkeeping.
    pub(in crate::ui) fn start(&mut self, target: String) {
        self.target = Some(target);
        self.awaiting = None;
        self.refetched = None;
    }

    /// Forget the per-level bookkeeping a prior target left, before a fresh intent decides how to reveal.
    pub(in crate::ui) fn reset_levels(&mut self) {
        self.awaiting = None;
        self.refetched = None;
    }

    /// End the walk: target and awaited level (a landing or a dead end; the refresh memo is kept).
    pub(in crate::ui) fn stop(&mut self) {
        self.target = None;
        self.awaiting = None;
    }

    /// End the walk and its refresh memo.
    pub(in crate::ui) fn abort(&mut self) {
        self.stop();
        self.refetched = None;
    }

    /// End everything, the root-armed target included (a row change, a failed result tree).
    pub(in crate::ui) fn clear(&mut self) {
        self.root_target = None;
        self.abort();
    }

    /// The awaited level's own reply arrived.
    pub(in crate::ui) fn level_answered(&mut self, ancestor: &str) {
        if self.awaiting.as_deref() == Some(ancestor) {
            self.awaiting = None;
        }
    }

    /// The walk requested `ancestor`'s children and waits on them.
    pub(in crate::ui) fn await_level(&mut self, ancestor: String) {
        self.awaiting = Some(ancestor);
    }

    /// The walk spent its one force-refresh on `(target, ancestor)`.
    pub(in crate::ui) fn mark_refetched(&mut self, key: (String, String)) {
        self.refetched = Some(key);
    }

    /// Arm `target` for its row's incoming Files root.
    pub(in crate::ui) fn arm_root(&mut self, target: String) {
        self.root_target = Some(target);
    }

    /// Defer an in-tree walk to the root reply: no walk in flight, `target` armed for the root. Returns whether a
    /// root was already armed (its request in flight, so no second one is sent).
    pub(in crate::ui) fn defer_to_root(&mut self, target: String) -> bool {
        self.target = None;
        self.root_target.replace(target).is_some()
    }

    /// The root request was not sent, or a mode change invalidates the armed target.
    pub(in crate::ui) fn cancel_root(&mut self) {
        self.root_target = None;
    }

    /// The row's root arrived: its armed target, once.
    pub(in crate::ui) fn take_root(&mut self) -> Option<String> {
        self.root_target.take()
    }
}
