//! What the window holds of its own computer's update, for the standing mark on the bottom border: the state the
//! local daemon last said, and where none spoke the window's own (`selfupdate::state`). No `impl State` lives here.

use crate::net::dial::HostKey;
use sot_protocol::{Mark, UpdateState};

/// The mark is one computer's word: the daemon's on the window's own computer, which shares the window's install, so
/// its update is the one this window's next start applies. Another computer's daemon is never shown (its state is in
/// `sotd status`). The word lives with its connection: a `Connected` begins with nothing said, the state follows it,
/// and a `Disconnected` forgets it, so the mark is what a connected daemon last said and a daemon that went away to
/// apply says so again when it is back. On a computer whose window is its only updater no local daemon speaks, and the
/// mark is the state of the window's own check.
pub(in crate::ui) struct UpdateMark {
    /// The dial host whose connection is this computer's own daemon, and what it has said on this connection.
    local: Option<(HostKey, Option<UpdateState>)>,
    /// A harness instance (`--ephemeral`, any `--capture`) draws no mark, so a screenshot never depends on what a
    /// release source answered.
    harness: bool,
}

impl UpdateMark {
    pub(in crate::ui) fn new(harness: bool) -> Self {
        Self { local: None, harness }
    }

    /// `host`'s connection came up; `local` when it is this computer's own daemon (`ResolvedDial::Local`).
    pub(in crate::ui) fn connected(&mut self, host: &HostKey, local: bool) {
        if local {
            self.local = Some((host.clone(), None));
        } else {
            self.disconnected(host);
        }
    }

    /// `host`'s daemon said the state of its update.
    pub(in crate::ui) fn told(&mut self, host: &HostKey, state: UpdateState) {
        if let Some((_, said)) = self.local.as_mut().filter(|(local, _)| local == host) {
            *said = Some(state);
        }
    }

    /// `host`'s connection ended.
    pub(in crate::ui) fn disconnected(&mut self, host: &HostKey) {
        if self.local.as_ref().is_some_and(|(local, _)| local == host) {
            self.local = None;
        }
    }

    /// The mark to draw, in the wire's words, or none: the local daemon's word where it has spoken on its live
    /// connection, else `own`, the state of this window's own update check.
    pub(in crate::ui) fn mark(&self, own: Option<UpdateState>) -> Option<Mark> {
        if self.harness {
            return None;
        }
        let said = self.local.as_ref().and_then(|(_, said)| said.clone());
        said.or(own)?.mark()
    }
}

#[cfg(test)]
#[path = "update_mark_tests.rs"]
mod tests;
