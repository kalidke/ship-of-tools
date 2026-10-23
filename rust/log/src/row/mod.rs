//! The row socket: one socket per row, `row-<h>`, speaking wire's frames. A viewer attaches here and sees the screen at
//! once, then the output in order; one connection is the driver and may type, the others only watch. Nothing is served
//! before an admitted hello. The row hello's `row` number, `ROW_PROTOCOL`, is the one gate between a row and a client:
//! no reader here judges a frame's `v`, so a daemon update never strands a live row.
//!
//! The daemon's four management calls (`row.status`, `row.end_run`, `row.reset`, `row.stop`) ride the same socket: the
//! row hands them to the [`Management`] its supervisor installs, and answers them `unknown_op` where none is.
//!
//! `row.watch` holds a connection open as the daemon's view of the row: it answers the status the row has, then each
//! change of phase or run is one `row.phase` event carrying the new status, in the order the changes happened, and a
//! controlled exit is told by `row.exiting {code}`, the last frame, before the stream ends. A watch takes no pen and needs
//! no attach, and is never closed as idle. A stream that ends with no `row.exiting` is a kill, a lost listener or an
//! exit before the bind: it carries no code.

mod client;
mod manage;
mod input;
mod proto;
mod run;
mod server;

pub use client::{PlatformConn, RowCanceler, RowClient, RowEvent, RowWaker};
pub use manage::Management;
pub use proto::{
    AttachReq, AttachRes, EndRunOutcome, EndRunReq, EndRunRes, ExitingEvt, HelloReq, HelloRes, InputRes, ResetOutcome,
    ResetRes, ResizeReq, RowError, RowHelloExchange, SnapshotEvt, StatusRes, StopOutcome, StopRes, OP_ATTACH, OP_DEMOTED,
    OP_END_RUN, OP_EXITING, OP_HELLO, OP_INPUT, OP_OUTPUT, OP_PHASE, OP_RESET, OP_RESIZE, OP_SNAPSHOT, OP_STATUS, OP_STOP,
    OP_TAKE, OP_WATCH, OUTPUT_CHUNK, SNAPSHOT_CHUNK,
};
pub use input::InputPump;
pub use run::{self_identity, ConnId, RowRequest, RowRun};
pub use server::{row_socket_path, RowServer};
#[cfg(unix)]
pub use server::socket_path_in;

/// The row socket's own protocol number. It rises only when a row op changes; a client refuses a different one by name.
pub const ROW_PROTOCOL: u32 = 1;

/// Connections one row serves at once.
pub const MAX_CONNECTIONS: usize = 8;
