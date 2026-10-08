//! A current attach request's one-shot receipt after its checkpointed pane frame is submitted and presented.

use std::time::{Duration, Instant};

/// What the frame knows of the client it lent the draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) struct PaneFacts {
    pub checkpointed: bool,
    pub attached: bool,
    pub live: bool,
}

/// A frame-local claim that the current request's pane was drawn; only the owner can make one.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) struct PresentationCandidate {
    generation: u64,
}

/// The outcome of offering a candidate after its frame.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) enum Presentation {
    Receipt { generation: u64, elapsed: Duration },
    /// The request has no known origin: there is no latency to report.
    NoOrigin,
    /// An older request's candidate, or one already consumed.
    Stale,
}

/// Owns the current attach request's generation and immutable origin.
#[derive(Debug, Default)]
pub(in crate::ui) struct PanePresentation {
    generation: u64,
    origin: Option<Instant>,
    completed: bool,
}

impl PanePresentation {
    /// A new attach request: a new generation, its origin fixed until the next request.
    pub(in crate::ui) fn begin(&mut self, origin: Option<Instant>) {
        self.generation += 1;
        self.origin = origin;
        self.completed = false;
    }

    /// A candidate for a client that is checkpointed, attached and live, drawn into a pane with area.
    /// Offering it completes nothing: only `complete`, after the frame is presented, does.
    pub(in crate::ui) fn candidate(&self, facts: PaneFacts, area: (u16, u16)) -> Option<PresentationCandidate> {
        let eligible = facts.checkpointed && facts.attached && facts.live && area.0 > 0 && area.1 > 0;
        (eligible && !self.completed).then_some(PresentationCandidate { generation: self.generation })
    }

    pub(in crate::ui) fn complete(&mut self, candidate: PresentationCandidate, now: Instant) -> Presentation {
        if candidate.generation != self.generation || self.completed {
            return Presentation::Stale;
        }
        self.completed = true;
        let Some(origin) = self.origin else {
            return Presentation::NoOrigin;
        };
        let elapsed = now.saturating_duration_since(origin);
        Presentation::Receipt { generation: candidate.generation, elapsed }
    }
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;
