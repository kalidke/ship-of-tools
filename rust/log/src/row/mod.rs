//! The row socket: one socket per row, `row-<h>`, speaking wire's frames. A viewer attaches here and sees the screen at
//! once, then the output in order; one connection is the driver and may type, the others only watch. Nothing is served
//! before an admitted hello. The row hello's `row` number, `ROW_PROTOCOL`, is the one gate between a row and a client:
//! no reader here judges a frame's `v`, so a daemon update never strands a live row.
//!
//! Beside the old voyage lane in this landing: nothing in the supervisor's start path reads this socket yet except
//! the run's output hook.

mod client;
mod proto;
mod run;
mod server;

pub use client::{RowClient, RowEvent};
pub use proto::{
    AttachReq, AttachRes, HelloReq, HelloRes, InputRes, ResizeReq, RowError, RowHelloExchange, SnapshotEvt, OP_ATTACH,
    OP_DEMOTED, OP_HELLO, OP_INPUT, OP_OUTPUT, OP_RESIZE, OP_SNAPSHOT, OP_TAKE, OUTPUT_CHUNK, SNAPSHOT_CHUNK,
};
pub use run::{self_identity, RowRequest, RowRun};
pub use server::{row_socket_path, socket_path_in, RowServer};

/// The row socket's own protocol number. It rises only when a row op changes; a client refuses a different one by name.
pub const ROW_PROTOCOL: u32 = 1;

/// Connections one row serves at once.
pub const MAX_CONNECTIONS: usize = 8;
