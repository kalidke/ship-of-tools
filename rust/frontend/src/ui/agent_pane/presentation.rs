//! Pane presentation ownership; the compatibility path retains the legacy selection-time completion until the fix lands.

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

    pub(in crate::ui) fn candidate(&self, _facts: PaneFacts, _area: (u16, u16)) -> Option<PresentationCandidate> {
        (!self.completed).then_some(PresentationCandidate { generation: self.generation })
    }

    pub(in crate::ui) fn complete(&mut self, candidate: PresentationCandidate, now: Instant) -> Presentation {
        if candidate.generation != self.generation || self.completed {
            return Presentation::Stale;
        }
        self.completed = true;
        let elapsed = self.origin.map(|o| now.saturating_duration_since(o)).unwrap_or_default();
        Presentation::Receipt { generation: candidate.generation, elapsed }
    }
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;
