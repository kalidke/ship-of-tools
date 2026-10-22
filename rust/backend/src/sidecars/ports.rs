//! The announcement of a loopback port a child serves: the capability a child's owner is given, the two events it
//! carries and the one receiver the daemon installs. Pages owns the receiver and its grant table (`pages::proxy`);
//! sidecars names only this module.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// What a child's owner tells the receiver. `owner` is the row's id and `generation` is the child's spawn generation
/// (`next_generation`), so an older child's event is told from its replacement's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortEvent {
    /// The child serves `port` on loopback: dial it.
    Allow { owner: String, generation: u64, port: u16 },
    /// The owner's children of this generation or older serve nothing now.
    End { owner: String, generation: u64 },
}

/// Why an event was not applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortRefused {
    /// The receiver already holds a newer generation for the owner (`current`).
    Stale { current: u64 },
    /// The receiver could not apply it, or there is none.
    Failed(String),
}

impl std::fmt::Display for PortRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stale { current } => write!(f, "a newer child (generation {current}) has replaced this one"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// The receiver of port announcements. `apply` returns once the event has taken effect or been refused: an answer of
/// `Ok` means the port is dialable (`Allow`) or no longer granted (`End`).
pub trait PortGrants: Send + Sync {
    fn apply(&self, event: PortEvent) -> Result<(), PortRefused>;
}

static INSTALLED: OnceLock<Arc<dyn PortGrants>> = OnceLock::new();

/// Install the process's receiver, once; a second call keeps the first.
pub fn install(receiver: Arc<dyn PortGrants>) {
    let _ = INSTALLED.set(receiver);
}

/// The installed receiver, if any.
pub(crate) fn installed() -> Option<Arc<dyn PortGrants>> {
    INSTALLED.get().cloned()
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The next spawn generation, from one counter for the process: a row's later REPL handle starts above the last, so
/// the receiver never mistakes a new handle's first child for a stale one.
pub(crate) fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// One child's end of the capability: its owner, its generation and the receiver (none installed is a refusal).
#[derive(Clone)]
pub(crate) struct PortSink {
    grants: Option<Arc<dyn PortGrants>>,
    owner: String,
    generation: u64,
}

impl PortSink {
    /// A sink for the row `workspace_id` (a REPL with none gets a fixed owner so its grants are still tracked and
    /// revoked), at generation 0 until `at`.
    pub(crate) fn new(grants: Option<Arc<dyn PortGrants>>, workspace_id: &Option<String>) -> Self {
        Self { grants, owner: workspace_id.clone().unwrap_or_else(|| "<legacy>".to_string()), generation: 0 }
    }

    pub(crate) fn at(&self, generation: u64) -> Self {
        Self { generation, ..self.clone() }
    }

    fn apply(&self, event: PortEvent) -> Result<(), PortRefused> {
        match &self.grants {
            Some(grants) => grants.apply(event),
            None => Err(PortRefused::Failed("no receiver of port announcements is installed".to_string())),
        }
    }

    /// Grant `port`; the caller forwards nothing that names it unless this is `Ok`.
    pub(crate) fn allow(&self, port: u16) -> Result<(), PortRefused> {
        self.apply(PortEvent::Allow { owner: self.owner.clone(), generation: self.generation, port })
    }

    /// End this generation's grants. A refusal is logged: an `End` has no frame to withhold.
    pub(crate) fn end(&self) {
        if let Err(refused) = self.apply(PortEvent::End { owner: self.owner.clone(), generation: self.generation }) {
            tracing::warn!(owner = %self.owner, generation = self.generation, %refused, "port grants not ended");
        }
    }
}
