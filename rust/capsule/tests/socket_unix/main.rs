#![cfg(unix)]
//! The client half of the Unix-domain-socket transport (`src/lane/socket_unix/`), against the row acceptor: a real
//! `RowServer` over a fake `RowRun`, or a plain listener where the peer must not answer. The server half of the lanes
//! is gone, and what the row acceptor keeps of its properties is tested with it (`tests/row.rs`: another uid is
//! dropped at accept; `tests/row_limits.rs`: a ninth connection is refused).
//!
//! Every test runs in its own process (`sot_platform::test_isolated::run_isolated`): a real process boundary bounds
//! every hang path, including one inside a wedged server's `drop` running after an earlier assertion panicked.

use sot_platform::transport::socket_unix::SocketClient;
use sot_platform::transport::{TransportError, CONNECT_BOUND};
use sot_capsule::row::{socket_path_in, RowRun, RowServer};
use sot_platform::test_isolated::run_isolated;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A per-step bound used throughout (well inside `sot_platform::test_isolated::ISOLATION_TIMEOUT`, so a stalled step
/// always trips before the parent's own kill fires).
const TIMEOUT: Duration = Duration::from_secs(10);

/// A fresh mode-0700 folder under `/tmp`: the row's folder must be private, and `$TMPDIR` on the macOS runner is ~56
/// bytes while `sun_path` is only 104 there, so the default would overflow the address for real on that leg.
fn private_tmp() -> tempfile::TempDir {
    let tmp = tempfile::Builder::new().prefix("sot-t").tempdir_in("/tmp").expect("tempdir under /tmp");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    tmp
}

fn row_path(dir: &Path) -> PathBuf {
    socket_path_in(dir, "0123456789abcdef").expect("a short path fits")
}

/// A row server over a fake run, and the folder it lives in.
fn bind_row() -> (tempfile::TempDir, PathBuf, RowServer) {
    let dir = private_tmp();
    let path = row_path(dir.path());
    let server = RowServer::bind(&path, RowRun::new("run-one", "ready")).expect("bind");
    (dir, path, server)
}

/// A client around a raw stream to `path`: no hello is sent, so the row says nothing until its hello deadline.
fn raw_client(path: &Path) -> Arc<SocketClient> {
    Arc::new(SocketClient::from_stream_for_test(UnixStream::connect(path).expect("connect"), 0))
}

mod client;
mod cloexec;
