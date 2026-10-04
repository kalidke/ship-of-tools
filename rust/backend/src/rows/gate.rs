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
    // The shutdown calls this; it lands after the gate.
    #[allow(dead_code)]
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
mod tests {
    use super::*;

    #[test]
    fn run_gate_table() {
        let reg = Workspaces::new();
        // Open: every id is admitted.
        assert!(reg.begin_start("ws-a-1").is_ok());
        assert!(reg.begin_start("ws-b-2").is_ok());

        // Settle returns true once a held permit drops.
        let permit = reg.begin_start("ws-b-2").expect("open gate");
        let dropper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(permit);
        });
        assert!(reg.close_gate_and_settle(Instant::now() + std::time::Duration::from_secs(5)));
        dropper.join().unwrap();

        // Closing refuses every id.
        for id in ["ws-a-1", "ws-b-2", "ws-new-3"] {
            let refused = reg.begin_start(id).err().expect("a closing gate refuses every start");
            assert!(refused.contains(id), "{refused}");
            assert!(refused.contains("this computer's backend is shutting down"), "{refused}");
        }

        // Settle returns false at the deadline while a permit is held.
        let reg = Workspaces::new();
        let _held = reg.begin_start("ws-a-1").expect("open gate");
        let started = Instant::now();
        assert!(!reg.close_gate_and_settle(started + std::time::Duration::from_millis(200)));
        assert!(started.elapsed() >= std::time::Duration::from_millis(200));
    }
}
