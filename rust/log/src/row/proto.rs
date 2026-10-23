//! The row socket's payloads, its op names and the hello's identity exchange.

use super::ROW_PROTOCOL;
use serde::{Deserialize, Serialize};
use sot_platform::identity::exchange::{ExchangeDecode, IdentityExchange};
use sot_protocol::{BlobDescriptor, Frame, Kind};

/// The most of a build label a hello carries.
const MAX_BUILD_BYTES: usize = 256;

pub const OP_HELLO: &str = "row.hello";
pub const OP_ATTACH: &str = "row.attach";
pub const OP_SNAPSHOT: &str = "row.snapshot";
pub const OP_OUTPUT: &str = "row.output";
pub const OP_TAKE: &str = "row.take";
pub const OP_INPUT: &str = "row.input";
pub const OP_RESIZE: &str = "row.resize";
pub const OP_DEMOTED: &str = "row.demoted";

/// A snapshot event's blob is at most this; a longer screen goes in several, the last flagged.
pub const SNAPSHOT_CHUNK: usize = 1024 * 1024;
/// An output event's blob is at most this.
pub const OUTPUT_CHUNK: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RowError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("the row socket path is {} bytes, past what a socket path holds: {}", .0.as_os_str().len(), .0.display())]
    PathTooLong(std::path::PathBuf),
    #[error("the row speaks row protocol {theirs}; this client speaks {ours}")]
    RowMismatch { ours: u32, theirs: u32 },
    #[error("the peer is not the row of this account")]
    Foreign,
    #[error("refused: {0}")]
    Refused(String),
    #[error("the peer could not be proven")]
    Unproven,
    #[error("delivery unknown: the row did not answer the input in time")]
    DeliveryUnknown,
    #[error("the connection is closed")]
    Closed,
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("protocol: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloReq {
    pub build: String,
    pub row: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloRes {
    pub pid: u32,
    pub created: u64,
    pub phase: String,
    pub run: String,
    pub row: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachReq {
    pub cols: u16,
    pub rows: u16,
    pub driver: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachRes {
    pub run: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEvt {
    pub last: bool,
    pub blob: BlobDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeReq {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputRes {
    pub written: u64,
}

/// The hello as the challenge's steps 4 and 5: the request is `row.hello`, the reply is the one frame that names the
/// row's `(pid, created)`. A reply carrying another row number ends the exchange foreign and is kept for the caller to
/// name; the frame's `v` is never read.
pub struct RowHelloExchange {
    build: String,
    buf: Vec<u8>,
    done: bool,
    mismatch: Option<u32>,
    reply: Option<HelloRes>,
}

impl RowHelloExchange {
    pub fn new(build: impl Into<String>) -> Self {
        // The build is a label; cut to a length that keeps the request far inside wire's cap.
        let mut build: String = build.into();
        if build.len() > MAX_BUILD_BYTES {
            let mut end = MAX_BUILD_BYTES;
            while !build.is_char_boundary(end) {
                end -= 1;
            }
            build.truncate(end);
        }
        Self { build, buf: Vec::new(), done: false, mismatch: None, reply: None }
    }

    /// The row number the peer declared, when it was not ours.
    pub fn mismatch(&self) -> Option<u32> {
        self.mismatch
    }

    /// The hello's reply, once the exchange proved the peer.
    pub fn reply(&self) -> Option<&HelloRes> {
        self.reply.as_ref()
    }
}

impl IdentityExchange for RowHelloExchange {
    fn encode_request(&self) -> Vec<u8> {
        let req = HelloReq { build: self.build.clone(), row: ROW_PROTOCOL };
        let frame = Frame::req(1, OP_HELLO, serde_json::to_value(req).expect("a hello serializes"));
        let mut line = serde_json::to_vec(&frame).expect("a frame serializes");
        line.push(b'\n');
        line
    }

    fn feed(&mut self, bytes: &[u8]) -> ExchangeDecode {
        if self.done {
            return ExchangeDecode::Foreign;
        }
        self.buf.extend_from_slice(bytes);
        let cap = sot_protocol::codec::MAX_ENVELOPE_BYTES;
        let Some(end) = self.buf.iter().position(|&b| b == b'\n') else {
            return if self.buf.len() > cap { self.finish(None) } else { ExchangeDecode::Incomplete };
        };
        // The cap is on the whole line, wherever its newline arrives; and one reply is all an exchange is owed, so
        // bytes behind it are corruption.
        if end > cap || end + 1 != self.buf.len() {
            return self.finish(None);
        }
        let Ok(frame) = serde_json::from_slice::<Frame>(&self.buf[..end]) else { return self.finish(None) };
        if frame.kind != Kind::Res || frame.op != OP_HELLO {
            return self.finish(None);
        }
        match serde_json::from_value::<HelloRes>(frame.payload) {
            Ok(res) if res.row != ROW_PROTOCOL => {
                self.mismatch = Some(res.row);
                self.finish(None)
            }
            Ok(res) => self.finish(Some(res)),
            Err(_) => self.finish(None),
        }
    }
}

impl RowHelloExchange {
    fn finish(&mut self, res: Option<HelloRes>) -> ExchangeDecode {
        self.done = true;
        match res {
            Some(res) => {
                let (pid, created) = (res.pid, res.created);
                self.reply = Some(res);
                ExchangeDecode::Identity { pid, created }
            }
            None => ExchangeDecode::Foreign,
        }
    }
}

#[cfg(test)]
#[path = "proto_tests.rs"]
mod tests;
