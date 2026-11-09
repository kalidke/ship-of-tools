//! The window's text selection share: a reconcile that keeps the daemon's published selection for a row equal to what the
//! preview shows selected. It holds privately the state it last asked for and the one request in flight, and returns the
//! request to send; it sends nothing itself. The request is never replayed (fe-net), so a lost connection is answered by
//! sending the state again after `Connected`.

use std::collections::HashSet;

use crate::net::dial::HostKey;
use crate::net::transport::{OutgoingReq, SelectionReply};
use sot_protocol::ops::{selection_code, SelectedText, SelectionSetReq, SelectionState, TextPos};
use sot_protocol::ops::{selection_digest, MAX_SELECTION_LEN};

/// What the view wants the daemon to hold: the selected text of one node, for one row on one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct Want {
    pub host: HostKey,
    pub workspace_id: String,
    pub selection: SelectedText,
}

impl Want {
    /// The released range `((line, byte), (line, byte))` and the text it covers, as the daemon is told of it. `None` for
    /// text the wire cannot describe (empty, over the daemon's read, or a position past `u32`).
    pub(in crate::ui) fn new(
        host: HostKey, workspace_id: String, node_id: String, range: ((usize, usize), (usize, usize)), text: &str,
    ) -> Option<Self> {
        let pos = |(line, byte): (usize, usize)| Some(TextPos { line: u32::try_from(line).ok()?, byte: u32::try_from(byte).ok()? });
        let len = u32::try_from(text.len()).ok().filter(|n| (1..=MAX_SELECTION_LEN).contains(n))?;
        let selection = SelectedText { node_id, start: pos(range.0)?, end: pos(range.1)?, len, digest: selection_digest(text) };
        Some(Self { host, workspace_id, selection })
    }
}

/// The one request waiting for its reply, and the state to restore if it was never written.
struct Flight {
    host: HostKey,
    req: SelectionSetReq,
    before: Option<Want>,
}

#[derive(Default)]
pub(in crate::ui) struct SelectionShare {
    /// Hosts whose connection is up; nothing is sent to another.
    up: HashSet<HostKey>,
    /// What the daemon was last asked to hold; a refusal is not retried until the desired state changes.
    sent: Option<Want>,
    flight: Option<Flight>,
}

impl SelectionShare {
    /// The next request toward `desired`, or `None` when the daemon already holds it, a request waits for its reply or the
    /// host that must be told is down. A row switch is a clear to the row that held the share, then the new row's share.
    pub(in crate::ui) fn reconcile(&mut self, desired: Option<Want>) -> Option<(HostKey, OutgoingReq)> {
        if self.flight.is_some() || self.sent == desired {
            return None;
        }
        let same_row = |a: &Want, b: &Want| a.host == b.host && a.workspace_id == b.workspace_id;
        // The row to tell: the one that holds a share we will not replace in place, else the desired row.
        let (host, workspace_id, selection, next) = match (&self.sent, desired) {
            (Some(held), want) if want.as_ref().is_none_or(|w| !same_row(held, w)) => (held.host.clone(), held.workspace_id.clone(), None, None),
            (_, Some(w)) => (w.host.clone(), w.workspace_id.clone(), Some(w.selection.clone()), Some(w)),
            (_, None) => return None,
        };
        if !self.up.contains(&host) {
            return None;
        }
        let req = SelectionSetReq { workspace_id, selection, drawing: None };
        let before = std::mem::replace(&mut self.sent, next);
        self.flight = Some(Flight { host: host.clone(), req: req.clone(), before });
        Some((host, OutgoingReq::SelectionSet { req }))
    }

    /// The request could not be queued: it is as if never sent, and the host reads as down until it connects again.
    pub(in crate::ui) fn send_failed(&mut self, host: &HostKey) {
        if let Some(f) = self.flight.take() {
            self.sent = f.before;
        }
        self.up.remove(host);
    }

    /// A link came or went. A going link forgets what the daemon held for that host (it withdraws a connection's
    /// selection at its end) and the request waiting on it, whose end reply then matches nothing.
    pub(in crate::ui) fn on_connection(&mut self, host: &HostKey, up: bool) {
        if up {
            self.up.insert(host.clone());
            return;
        }
        self.up.remove(host);
        if self.sent.as_ref().is_some_and(|w| &w.host == host) {
            self.sent = None;
        }
        if self.flight.as_ref().is_some_and(|f| &f.host == host) {
            self.flight = None;
        }
    }

    /// A reply: the status line it earns, if any. A reply that does not answer the request in flight on that host (one from
    /// a connection that ended) is dropped. An ended reply shows nothing; the reconcile sends again after `Connected`.
    pub(in crate::ui) fn on_reply(&mut self, host: &HostKey, req: &SelectionSetReq, reply: SelectionReply) -> Option<String> {
        if !self.flight.as_ref().is_some_and(|f| &f.host == host && &f.req == req) {
            return None;
        }
        self.flight = None;
        // A clear is not announced, and a refused one leaves nothing to retry.
        let share = req.selection.is_some();
        match reply {
            SelectionReply::Ended => {
                // The daemon withdrew it at the connection's end; the host reads as down until `Connected`.
                self.sent = None;
                self.up.remove(host);
                None
            }
            SelectionReply::Done(SelectionState::Shared) => share.then(|| "selection shared: the agent sees it at its next prompt".into()),
            SelectionReply::Done(SelectionState::Held) => {
                share.then(|| "selection held: this row's agent started before it could take one; restart the row to share".into())
            }
            SelectionReply::Done(SelectionState::Other) => share.then(|| "selection not shared: unknown answer".into()),
            SelectionReply::Unsupported => share.then(|| "selection not shared: this host's daemon does not take selections".into()),
            SelectionReply::Refused { code, error } => share.then(|| match code.as_deref() {
                Some(selection_code::NOT_FILE_TEXT) => "selection not shared: this view is not the file's text".into(),
                Some(selection_code::NO_AGENT) => "selection not shared: this row's agent takes none".into(),
                _ => format!("selection not shared: {error}"),
            }),
        }
    }
}

impl super::super::State {
    /// What the view shows selected now: the selected row's released, non-empty text selection over its installed
    /// preview. The refused-hello overlay and edit mode show no preview text, and a row with no workspace id (the
    /// daemon's default) is never shared.
    fn desired_selection(&self) -> Option<Want> {
        if self.editor.is_editing() || self.shaped.fatal().is_some() {
            return None;
        }
        let (range, text) = self.shaped.released_selection()?;
        Want::new(
            self.selected.host().clone(), self.selected.workspace_id().clone()?, self.preview_source.node_id()?.to_string(), range, &text,
        )
    }

    /// Run in the frame's upkeep after `drain_events`: sends the one request that moves the daemon toward the view.
    pub(in crate::ui) fn reconcile_selection_share(&mut self) {
        let desired = self.desired_selection();
        if let Some((host, req)) = self.selection_share.reconcile(desired) {
            if let Err(e) = self.send_to(&host, req) {
                tracing::warn!(error = %e, %host, "drop selection.set — channel closed");
                self.selection_share.send_failed(&host);
            }
        }
    }

    pub(in crate::ui) fn on_selection_reply(&mut self, host: &HostKey, req: &SelectionSetReq, reply: SelectionReply) {
        if let Some(line) = self.selection_share.on_reply(host, req, reply) {
            self.status.set(line);
        }
    }
}

#[cfg(test)]
#[path = "share_tests.rs"]
mod tests;
