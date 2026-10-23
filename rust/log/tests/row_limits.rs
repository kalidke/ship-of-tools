#![cfg(target_os = "linux")]
//! The row socket's eight connections are eight real ones: a connection's slot returns only when both of its threads
//! have ended, an evicted viewer's threads end, and a shut-down server leaves none. Serial, because it counts the
//! process's threads.

use sot_log::row::{socket_path_in, RowClient, RowRun, RowServer};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static SERIAL: Mutex<()> = Mutex::new(());
const BOUND: Duration = Duration::from_secs(5);

fn row_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|t| std::fs::read_to_string(t.unwrap().path().join("comm")).ok())
        .filter(|name| name.starts_with("row-conn") || name.starts_with("row-write"))
        .count()
}

fn bind(dir: &std::path::Path) -> (Arc<RowRun>, RowServer, std::path::PathBuf) {
    // The challenge refuses a peer that started in the tick it connects in; this binary is both ends.
    std::thread::sleep(Duration::from_millis(50));
    let path = socket_path_in(dir, "0123456789abcdef").unwrap();
    let run = RowRun::new("run-one", "ready");
    let server = RowServer::bind(&path, Arc::clone(&run)).expect("bind");
    (run, server, path)
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_ninth_connection_is_refused_while_eight_are_held() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (run, server, path) = bind(dir.path());
    let held: Vec<RowClient> = (0..8).map(|_| RowClient::connect(&path, "t", BOUND).expect("one of eight")).collect();
    assert_eq!(run.open_connections(), 8);
    assert!(RowClient::connect(&path, "t", Duration::from_secs(2)).is_err(), "a ninth is refused");
    assert_eq!(run.open_connections(), 8);
    drop(held);
    server.shutdown();
}

#[test]
fn an_evicted_viewer_gives_its_slot_back_only_when_its_threads_have_ended() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (run, server, path) = bind(dir.path());
    let parser = vt100_ctt::Parser::new(24, 80, 0);
    let idle: Vec<RowClient> = (0..7).map(|_| RowClient::connect(&path, "t", BOUND).unwrap()).collect();
    let mut slow = RowClient::connect(&path, "t", BOUND).unwrap();
    slow.attach(80, 24, false).unwrap();
    run.tick(&parser);
    // The viewer reads nothing: its client's buffer, the socket's and the connection's queue fill, and it is evicted.
    let chunk = vec![b'x'; 60 * 1024];
    let mut p = vt100_ctt::Parser::new(24, 80, 0);
    // The client holds 256 frames and the connection 256 more before either gives: flood until it does.
    for _ in 0..4000 {
        p.process(&chunk[..1]);
        run.output(&chunk, &p);
        if run.open_connections() == 7 {
            break;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    wait_for("the evicted viewer's slot to return", || run.open_connections() == 7);
    // Both of its threads ended with it: two threads per open connection, no more.
    wait_for("the evicted viewer's threads to end", || row_threads() == 14);
    let ninth = RowClient::connect(&path, "t", BOUND).expect("the slot is free again");
    assert_eq!(run.open_connections(), 8);
    drop((idle, ninth, slow));
    server.shutdown();
}

#[test]
fn after_shutdown_no_connection_thread_is_left() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (run, server, path) = bind(dir.path());
    let clients: Vec<RowClient> = (0..3).map(|_| RowClient::connect(&path, "t", BOUND).unwrap()).collect();
    assert_eq!(row_threads(), 6);
    server.shutdown();
    assert_eq!(run.open_connections(), 0);
    // A joined thread can stay in /proc/self/task for a moment: its join returns when the kernel clears its id, before
    // the task is reaped. So the count is read until it reaches zero, within a bound far shorter than any thread left
    // blocked on a read would take to end (the clients are still open).
    let deadline = Instant::now() + Duration::from_secs(1);
    while row_threads() != 0 {
        assert!(Instant::now() < deadline, "shutdown joins every connection's threads: {} left", row_threads());
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(clients);
}
