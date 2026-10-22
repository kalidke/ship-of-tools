//! The acceptor and the connection threads.

use super::proto::RowError;
use super::run::RowRun;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct RowServer {
    _private: (),
}

/// `<runtime_dir>/row-<h>.sock`.
pub fn row_socket_path(_h: &str) -> Result<PathBuf, RowError> {
    todo!()
}

/// `<dir>/row-<h>.sock`, refused by name when it is longer than a socket path holds.
pub fn socket_path_in(_dir: &Path, _h: &str) -> Result<PathBuf, RowError> {
    todo!()
}

impl RowServer {
    pub fn bind(_path: &Path, _run: Arc<RowRun>) -> Result<Self, RowError> {
        todo!()
    }
    /// A server that takes this uid for its own, so a test's peer reads as another account.
    #[doc(hidden)]
    pub fn bind_as_uid_for_test(_path: &Path, _run: Arc<RowRun>, _uid: u32) -> Result<Self, RowError> {
        todo!()
    }
    pub fn shutdown(self) {
        todo!()
    }
}
