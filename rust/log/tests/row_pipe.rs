#![cfg(windows)]
//! The row socket on a Windows pipe (pass 6c-1a1, proven on the hosted run): every instance the listener creates while
//! several clients connect at once must pass the client's own challenge, which refuses a pipe served by another account,
//! and each is served the hello. The descriptor is `owner_protected_pipe_descriptor`, given to the listener once; that
//! interprocess applies it to every instance it creates is premise U1.

use sot_log::row::{row_socket_path, RowClient, RowRun, RowServer};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn every_pipe_instance_is_owner_only() {
    let h = format!("test{}", std::process::id());
    let path = row_socket_path(&h).expect("the pipe name");
    let run = RowRun::new("run-one", "ready");
    let server = RowServer::bind(&path, Arc::clone(&run)).expect("bind the pipe");
    // Several connections at once force the listener to create further instances after the first.
    let clients: Vec<RowClient> = (0..4)
        .map(|_| RowClient::connect(&path, "test", Duration::from_secs(10)).expect("each instance is owner-only and answers"))
        .collect();
    assert!(clients.iter().all(|c| c.run() == "run-one"));
    drop(clients);
    server.shutdown();
}
