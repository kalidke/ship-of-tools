//! The blocking row client: one connection, a reader thread, requests answered in order.

use super::proto::RowError;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowEvent {
    /// A piece of the snapshot; `last` ends it.
    Snapshot { bytes: Vec<u8>, last: bool },
    Output(Vec<u8>),
    /// Another connection took the driver's place.
    Demoted,
}

pub struct RowClient {
    _private: (),
}

impl RowClient {
    /// Dials, proves the peer and checks its row number, all within `bound`.
    pub fn connect(_path: &Path, _build: &str, _bound: Duration) -> Result<Self, RowError> {
        todo!()
    }
    /// The run the row declared in its hello.
    pub fn run(&self) -> &str {
        todo!()
    }
    pub fn attach(&mut self, _cols: u16, _rows: u16, _driver: bool) -> Result<String, RowError> {
        todo!()
    }
    pub fn next_event(&mut self, _wait: Duration) -> Result<Option<RowEvent>, RowError> {
        todo!()
    }
    pub fn take(&mut self) -> Result<(), RowError> {
        todo!()
    }
    pub fn send_input(&mut self, _bytes: &[u8]) -> Result<usize, RowError> {
        todo!()
    }
    pub fn resize(&mut self, _cols: u16, _rows: u16) -> Result<(), RowError> {
        todo!()
    }
}
