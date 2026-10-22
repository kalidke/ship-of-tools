//! The run side of the row socket: what the supervisor's main loop feeds and drains.

use std::sync::{mpsc, Arc};

/// A request a connection hands the main loop; the loop does it and answers.
pub enum RowRequest {
    Input { bytes: Vec<u8>, reply: mpsc::SyncSender<Result<usize, String>> },
    Resize { cols: u16, rows: u16 },
}

/// One run's fan-out. The main loop calls [`RowRun::output`] after the parser took a chunk, [`RowRun::tick`] every pass,
/// and drains [`RowRun::take_requests`]; connections never touch the parser or the terminal.
pub struct RowRun {
    _private: (),
}

impl RowRun {
    pub fn new(_run: impl Into<String>, _phase: &str) -> Arc<Self> {
        todo!()
    }
    pub fn run_id(&self) -> String {
        todo!()
    }
    pub fn set_phase(&self, _phase: &str) {
        todo!()
    }
    /// Called with the chunk the parser just took.
    pub fn output(&self, _bytes: &[u8], _parser: &vt100_ctt::Parser) {
        todo!()
    }
    /// Serves attaches waiting for a ground state.
    pub fn tick(&self, _parser: &vt100_ctt::Parser) {
        todo!()
    }
    pub fn take_requests(&self) -> Vec<RowRequest> {
        todo!()
    }
    /// What the loop calls to be woken when a request arrives.
    pub fn set_wake(&self, _wake: Box<dyn Fn() + Send + Sync>) {
        todo!()
    }
}

/// This process's `(pid, created)`, the identity a hello answers with.
pub fn self_identity() -> crate::Result<(u32, u64)> {
    todo!()
}
