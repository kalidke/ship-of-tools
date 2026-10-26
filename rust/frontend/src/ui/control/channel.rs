//! The control channel's window half: the queue the command-file watcher fills and the signature of the last
//! `fe-state.json` written (ADR 0019).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::FeCommand;

#[derive(Default)]
pub(in crate::ui) struct ControlChannel {
    /// FE control commands (ADR 0019) enqueued by the command-file watcher
    /// thread (the producer) and drained on the main thread in `window_event`
    /// (the consumer), so dispatch runs the same code paths as the keybinds.
    commands: Arc<Mutex<VecDeque<FeCommand>>>,
    /// Hash of the last `fe-state.json` we wrote (ADR 0019), so the readback
    /// file is only rewritten when the observable state actually changes.
    state_sig: Option<u64>,
}

impl ControlChannel {
    /// The producer's end, for the command-file watcher thread.
    pub(in crate::ui) fn producer(&self) -> Arc<Mutex<VecDeque<FeCommand>>> { self.commands.clone() }

    /// Everything enqueued so far, taken under the lock alone (a poisoned queue yields nothing), so dispatch never
    /// holds it.
    pub(in crate::ui) fn drain(&self) -> Vec<FeCommand> {
        match self.commands.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Whether `sig` is the state last written.
    pub(in crate::ui) fn state_written(&self, sig: u64) -> bool { self.state_sig == Some(sig) }
    pub(in crate::ui) fn record_state(&mut self, sig: u64) { self.state_sig = Some(sig); }
}
