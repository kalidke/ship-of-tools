//! The window's selection share: a reconcile that keeps the daemon's published selection for a row equal to what the
//! view shows selected, the preview's released text or the drawing over the shown base. It holds privately the state it last asked for and the one request in flight, and returns the
//! request to send; it sends nothing itself. The request is never replayed (fe-net), so a lost connection is answered by
//! sending the state again after `Connected`.

use std::collections::HashSet;

use crate::net::dial::HostKey;
use crate::net::transport::{OutgoingReq, SelectionReply};
use crate::ui::preview::image::ink::InkLayer;
use sot_protocol::ops::{selection_code, Drawing, InkBase, SelectedText, SelectionSetReq, SelectionState, TextPos};
use sot_protocol::ops::{selection_digest, MAX_SELECTION_LEN};

/// What is shared: the selected text of one node, or the drawing on a base, identified by the owner's revision (the
/// strokes travel only in the request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum Share {
    Text(SelectedText),
    Ink { base: InkBase, revision: u64 },
}

/// What the view wants the daemon to hold, for one row on one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct Want {
    pub host: HostKey,
    pub workspace_id: String,
    pub share: Share,
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
        Some(Self { host, workspace_id, share: Share::Text(selection) })
    }
}

/// The one request waiting for its reply.
struct Flight {
    host: HostKey,
    req: SelectionSetReq,
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
    /// Hands `send` the next request toward `desired`; nothing when the daemon already holds it, a request waits for its
    /// reply or the host that must be told is down. A row switch is a clear to the row that held the share, then the new
    /// row's share. A send that fails is as a connection's end: the host reads as down and its share is forgotten.
    pub(in crate::ui) fn reconcile<E>(
        &mut self, desired: Option<Want>, strokes: &[Vec<[u16; 2]>], send: impl FnOnce(&HostKey, OutgoingReq) -> Result<(), E>,
    ) {
        if self.flight.is_some() || self.sent == desired {
            return;
        }
        let same_row = |a: &Want, b: &Want| a.host == b.host && a.workspace_id == b.workspace_id;
        // The row to tell: the one that holds a share we will not replace in place, else the desired row.
        let (host, workspace_id, next) = match (&self.sent, desired) {
            (Some(held), want) if want.as_ref().is_none_or(|w| !same_row(held, w)) => (held.host.clone(), held.workspace_id.clone(), None),
            (_, Some(w)) => (w.host.clone(), w.workspace_id.clone(), Some(w)),
            (_, None) => return,
        };
        if !self.up.contains(&host) {
            return;
        }
        let (selection, drawing) = match next.as_ref().map(|w| &w.share) {
            Some(Share::Text(t)) => (Some(t.clone()), None),
            Some(Share::Ink { base, .. }) => (None, Some(Drawing { base: base.clone(), strokes: strokes.to_vec() })),
            None => (None, None),
        };
        let req = SelectionSetReq { workspace_id, selection, drawing };
        self.sent = next;
        self.flight = Some(Flight { host: host.clone(), req: req.clone() });
        if send(&host, OutgoingReq::SelectionSet { req }).is_err() {
            self.on_connection(&host, false);
        }
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
            reply if req.drawing.is_some() => drawing_line(reply),
            SelectionReply::Done(SelectionState::Shared) => share.then(|| "selection shared: the agent sees it at its next prompt".into()),
            SelectionReply::Done(SelectionState::Held) => {
                share.then(|| "selection held: this row's agent started before it could take one; restart the row to share".into())
            }
            SelectionReply::Done(SelectionState::Other) => share.then(|| "selection not shared: unknown answer".into()),
            SelectionReply::Unsupported => share.then(|| "selection not shared: this host's daemon does not take selections".into()),
            SelectionReply::Refused { code, error } => share.then(|| match code.as_deref() {
                Some(selection_code::NOT_FILE_TEXT) => "selection not shared: this view is not the file's text".into(),
                Some(selection_code::NO_AGENT) => "selection not shared: this row's agent takes none".into(),
                _ => format!("selection not shared: {}", code.as_deref().unwrap_or(&error)),
            }),
        }
    }
}

/// The status line a reply to a shared drawing earns.
fn drawing_line(reply: SelectionReply) -> Option<String> {
    Some(match reply {
        SelectionReply::Done(SelectionState::Shared) => "drawing shared: the agent sees it at its next prompt".into(),
        SelectionReply::Done(SelectionState::Held) => "drawing held: this row's agent started before it could take one; restart the row to share".into(),
        SelectionReply::Done(SelectionState::Other) => "drawing not shared: unknown answer".into(),
        SelectionReply::Unsupported => "drawing not shared: this daemon cannot take a drawing; update it".into(),
        SelectionReply::Refused { code, error } => match code.as_deref() {
            Some(selection_code::TOO_LARGE) => "drawing not shared: the image is over 40 megapixels".into(),
            Some(selection_code::NOT_AN_IMAGE) => "drawing not shared: the daemon cannot read this image".into(),
            Some(selection_code::NO_AGENT) => "drawing not shared: this row's agent takes none".into(),
            _ => format!("drawing not shared: {}", code.as_deref().unwrap_or(&error)),
        },
        SelectionReply::Ended => return None,
    })
}

/// What the view wants the daemon to hold: the released text selection (its range and text) over the preview's node,
/// for the selected row. None when the text preview is not shown, the row has no workspace id (the daemon's default), the
/// preview has no node, or no released range (mid-drag or none).
fn wanted(
    shown: bool, host: &HostKey, row: Option<String>, node: Option<&str>, released: Option<(((usize, usize), (usize, usize)), String)>,
) -> Option<Want> {
    if !shown {
        return None;
    }
    let (range, text) = released?;
    Want::new(host.clone(), row?, node?.to_string(), range, &text)
}

/// The drawing the view wants shared: the row's committed strokes, while the base they are on is the one shown. Read from
/// the owner in one call, so the base, the strokes and the revision are one drawing.
fn wanted_ink(shown: Option<InkBase>, ink: &InkLayer, host: &HostKey, row: Option<String>) -> Option<Want> {
    let base = shown.filter(|b| !ink.strokes().is_empty() && ink.base() == Some(b))?;
    Some(Want { host: host.clone(), workspace_id: row?, share: Share::Ink { base, revision: ink.revision() } })
}

/// One frame's step of the share: nothing while a stroke or an erase drag is live (the drag sends once, at its end), else
/// the request that moves the daemon toward the view. A drawing wins over a text selection (an image or the canvas shows
/// no flowed text).
fn step<E>(
    share: &mut SelectionShare, ink: &InkLayer, shown: Option<InkBase>, host: &HostKey, row: Option<String>, text: Option<Want>,
    send: impl FnOnce(&HostKey, OutgoingReq) -> Result<(), E>,
) {
    if ink.busy() {
        return;
    }
    let desired = wanted_ink(shown, ink, host, row).or(text);
    share.reconcile(desired, ink.strokes(), send);
}

impl super::super::State {
    /// The released text selection over the installed preview, gated on the predicate the paint and the press use
    /// (`preview_shows`), for the selected row; the drawing wins over it in `step`.
    fn desired_text(&self) -> Option<Want> {
        let shown = self.preview_shows().4;
        wanted(shown, self.selected.host(), self.selected.workspace_id().clone(), self.preview_source.node_id(), self.shaped.released_selection())
    }

    /// Run in the frame's upkeep after `drain_events`: sends the one request that moves the daemon toward the view.
    pub(in crate::ui) fn reconcile_selection_share(&mut self) {
        let (text, shown) = (self.desired_text(), self.shown_ink_base());
        let mut share = std::mem::take(&mut self.selection_share);
        let host = self.selected.host().clone();
        step(&mut share, &self.ink, shown, &host, self.selected.workspace_id().clone(), text, |host, req| {
            self.send_to(host, req).inspect_err(|e| tracing::warn!(error = %e, %host, "drop selection.set — channel closed"))
        });
        self.selection_share = share;
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
