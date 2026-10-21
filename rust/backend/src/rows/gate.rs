//! The run gate: whether a capsule run may start, and the permits in flight.

use std::time::Instant;

use super::*;

/// Which run starts may begin. Every start of a capsule run
/// takes a [`StartPermit`] at its primitive (`start_supervisor`, the
/// watchdog's restart, `reset_run`), so the shutdown can close the gate
/// and then wait for the starts already past it.
#[derive(Default)]
pub(super) struct RunGate {
    /// Set once by the shutdown; refuses every start from then on.
    closing: bool,
    /// Permits not yet dropped.
    in_flight: usize,
}

impl RunGate {
    /// Why no start may begin now, if none may.
    fn refusal(&self) -> Option<&'static str> {
        self.closing.then_some("this computer's backend is shutting down")
    }
}

/// One run start in flight. Dropping it ends the start as far as the gate
/// is concerned.
pub struct StartPermit(Arc<(Mutex<RunGate>, Condvar)>);

impl Drop for StartPermit {
    fn drop(&mut self) {
        let (gate, settled) = &*self.0;
        gate.lock().unwrap_or_else(|e| e.into_inner()).in_flight -= 1;
        settled.notify_all();
    }
}

impl Workspaces {
    /// Admits one run start for `workspace_id`, or refuses it with a text
    /// that names the workspace and the reason.
    pub fn begin_start(&self, workspace_id: &str) -> Result<StartPermit, String> {
        let mut gate = self.gate.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = gate.refusal() {
            return Err(format!("workspace {workspace_id} cannot start: {reason}"));
        }
        gate.in_flight += 1;
        Ok(StartPermit(self.gate.clone()))
    }

    /// Closes the gate for good, then waits until every permit has dropped
    /// or `deadline` passes. BLOCKING. True iff no start is still in flight.
    pub fn close_gate_and_settle(&self, deadline: Instant) -> bool {
        let (gate, settled) = &*self.gate;
        let mut gate = gate.lock().unwrap_or_else(|e| e.into_inner());
        gate.closing = true;
        while gate.in_flight > 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else {
                return false;
            };
            gate = settled.wait_timeout(gate, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        true
    }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
