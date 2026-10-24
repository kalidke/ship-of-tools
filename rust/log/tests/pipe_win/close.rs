//! PipeClient tests: cancel from another thread, concurrent submit.

use super::*;
use sot_platform::transport::connect_own::connect_own;
use sot_platform::transport::TransportError;
use std::sync::Arc;

/// A `PipeClient::read` blocked on one thread is unblocked by `cancel()` called from another, returning
/// `TransportError::Cancelled`.
#[test]
fn client_read_cancel_unblocks_from_another_thread() {
    if !run_isolated("close::client_read_cancel_unblocks_from_another_thread") {
        return;
    }
    let (path, _server, _run) = bind_row();
    let client = Arc::new(connect_own(&path).unwrap());

    let reader_client = Arc::clone(&client);
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        reader_client.read(&mut buf) // blocks: the row answers nothing before a hello
    });
    std::thread::sleep(Duration::from_millis(300)); // let the read actually become Pending
    client.cancel();

    let result = sot_platform::test_wait::within(TIMEOUT, "the cancelled reader", move || reader.join()).unwrap();
    assert!(matches!(result, Err(TransportError::Cancelled)), "expected Cancelled, got {result:?}");
}

/// A `PipeClient::write_all` blocked on one thread (the pipe's buffer saturated because nobody reads it) is unblocked
/// by `cancel()` called from another.
#[test]
fn client_write_cancel_unblocks_from_another_thread() {
    if !run_isolated("close::client_write_cancel_unblocks_from_another_thread") {
        return;
    }
    // A pipe nobody reads: the row would end a connection that sends it a flood.
    let path = fresh_row_pipe();
    let _silent = silent_pipe(&path);
    let client = Arc::new(connect_own(&path).unwrap());

    let writer_client = Arc::clone(&client);
    let writer = std::thread::spawn(move || {
        let payload = vec![0xCDu8; 65_536];
        loop {
            if let Err(e) = writer_client.write_all(&payload) {
                return e;
            }
        }
    });
    std::thread::sleep(Duration::from_secs(2)); // let the flood saturate the pipe buffer
    client.cancel();

    let result = sot_platform::test_wait::within(TIMEOUT, "the cancelled writer", move || writer.join()).unwrap();
    assert!(matches!(result, TransportError::Cancelled), "expected Cancelled, got {result:?}");
}

/// A SECOND concurrent same-direction `PipeClient::read` returns `TransportError::ConcurrentSubmit` rather than
/// racing the first caller's `OVERLAPPED`.
#[test]
fn concurrent_same_direction_client_read_returns_distinct_error() {
    if !run_isolated("close::concurrent_same_direction_client_read_returns_distinct_error") {
        return;
    }
    let (path, _server, _run) = bind_row();
    let client = Arc::new(connect_own(&path).unwrap());

    let a = Arc::clone(&client);
    let reader_a = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        a.read(&mut buf) // blocks: nobody ever sends
    });
    std::thread::sleep(Duration::from_millis(300)); // let A's read actually become Pending

    let b = Arc::clone(&client);
    let result_b = sot_platform::test_wait::within(TIMEOUT, "the refused second read", move || {
        let mut buf_b = [0u8; 16];
        b.read(&mut buf_b)
    });
    assert!(matches!(result_b, Err(TransportError::ConcurrentSubmit)), "expected ConcurrentSubmit, got {result_b:?}");

    client.cancel();
    let result_a = sot_platform::test_wait::within(TIMEOUT, "the cancelled first reader", move || reader_a.join()).unwrap();
    assert!(matches!(result_a, Err(TransportError::Cancelled)), "expected Cancelled, got {result_a:?}");
}
