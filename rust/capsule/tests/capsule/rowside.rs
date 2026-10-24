//! A run's row socket for a test: the `RowRun` the run feeds, served on a path of the test's own, and the clients that
//! attach to it, the way every shipped viewer does.

use super::*;
use sot_capsule::capsule::RunSignals;
use sot_capsule::row::{RowClient, RowEvent, RowRun, RowServer};
use std::sync::atomic::AtomicU64;

pub struct RowSide {
    /// Goes in `CapsuleConfig::signals`: names the row the run feeds.
    pub signals: Arc<RunSignals>,
    server: Option<RowServer>,
    path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl RowSide {
    pub fn new() -> Self {
        // The challenge refuses a peer that started in the clock tick it connects in (a pid reused inside one tick
        // cannot be told apart), and this test binary's one process is both ends: the first test waits until the
        // process is older.
        static AGED: std::sync::Once = std::sync::Once::new();
        AGED.call_once(|| std::thread::sleep(Duration::from_millis(50)));
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!("cap{}x{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = sot_capsule::row::socket_path_in(dir.path(), &name).expect("a short path fits");
        #[cfg(windows)]
        let path = sot_capsule::row::row_socket_path(&name).expect("a pipe name");
        let row = RowRun::new("run-one", "ready");
        let signals = Arc::new(RunSignals::default());
        signals.attach_row(Arc::clone(&row));
        let server = RowServer::bind(&path, row).expect("bind the row socket");
        Self { signals, server: Some(server), path, _dir: dir }
    }

    /// A client attached as the driver, at the run's own geometry.
    pub fn driver(&self, cols: u16, rows: u16) -> RowClient {
        self.client(cols, rows, true)
    }

    /// A client attached as a watcher.
    pub fn watcher(&self, cols: u16, rows: u16) -> RowClient {
        self.client(cols, rows, false)
    }

    fn client(&self, cols: u16, rows: u16, driver: bool) -> RowClient {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match RowClient::connect(&self.path, "test", Duration::from_secs(5)) {
                Ok(mut client) => {
                    client.attach(cols, rows, driver).expect("attach to the row");
                    return client;
                }
                Err(e) => assert!(Instant::now() < deadline, "the row socket never answered: {e}"),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for RowSide {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.shutdown();
        }
    }
}

/// Reads events until `done` says what has been seen is enough, and returns it: the screen of each snapshot that ended
/// and every live output byte, in order. Panics with what was seen when `within` passes.
pub fn read_until(client: &mut RowClient, what: &str, within: Duration, done: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match client.next_event(Duration::from_millis(100)).expect("the row stays up") {
            Some(RowEvent::Output(bytes)) => seen.extend_from_slice(&bytes),
            Some(RowEvent::Snapshot { bytes, last }) => {
                if last {
                    let mut parser = vt100_ctt::Parser::new(24, 80, 0);
                    parser.restore_screen(&bytes).expect("the snapshot restores");
                    seen.extend_from_slice(parser.screen().contents().as_bytes());
                }
            }
            _ => {}
        }
        if done(&String::from_utf8_lossy(&seen)) {
            return String::from_utf8_lossy(&seen).into_owned();
        }
    }
    panic!("no {what} within {within:?}; saw {:?}", String::from_utf8_lossy(&seen));
}

/// Reads until `needle` shows.
pub fn read_until_contains(client: &mut RowClient, needle: &str, within: Duration) -> String {
    read_until(client, &format!("{needle:?}"), within, |seen| seen.contains(needle))
}
