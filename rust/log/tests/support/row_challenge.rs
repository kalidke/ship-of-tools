//! Exchanges the challenge tests drive against a real row server, each a way the hello can go wrong. The row answers
//! the hello itself, so a peer that must answer wrongly, late or never is made by what the client sends and what it
//! accepts back: no stand-in server is needed, and the same cases run on every system.

// Each test file uses the cases it needs.
#![allow(dead_code)]

use sot_log::row::RowHelloExchange;
use sot_platform::identity::exchange::{ExchangeDecode, IdentityExchange};

/// The row's own hello, proven as another process: whatever identity the row names is replaced by `(pid, created)`.
pub struct Lying {
    inner: RowHelloExchange,
    pid: u32,
    created: u64,
}

impl Lying {
    pub fn new(pid: u32, created: u64) -> Self {
        Self { inner: RowHelloExchange::new("test"), pid, created }
    }
}

impl IdentityExchange for Lying {
    fn encode_request(&self) -> Vec<u8> {
        self.inner.encode_request()
    }

    fn feed(&mut self, bytes: &[u8]) -> ExchangeDecode {
        match self.inner.feed(bytes) {
            ExchangeDecode::Identity { .. } => ExchangeDecode::Identity { pid: self.pid, created: self.created },
            other => other,
        }
    }
}

/// Half a frame with no end: the row waits for the rest and answers nothing, so the client's read stays pending.
pub struct Unanswered;

impl IdentityExchange for Unanswered {
    fn encode_request(&self) -> Vec<u8> {
        b"{\"v\":".to_vec()
    }

    fn feed(&mut self, _bytes: &[u8]) -> ExchangeDecode {
        ExchangeDecode::Incomplete
    }
}

/// A line that is no frame: the row ends the connection without a reply.
pub struct NotAFrame;

impl IdentityExchange for NotAFrame {
    fn encode_request(&self) -> Vec<u8> {
        b"this is no frame\n".to_vec()
    }

    fn feed(&mut self, _bytes: &[u8]) -> ExchangeDecode {
        ExchangeDecode::Incomplete
    }
}
