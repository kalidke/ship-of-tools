// link.*, the window's one-login link (topology's `link` module runs it)

use super::*;

/// The link's own number, declared by `link.hello` and raised only when the link family changes. It gates the window
/// and the far router; the protocol integer gates the hellos inside the channels, the row-protocol number the rows.
pub const LINK: u32 = 1;
/// The most bytes one `link.data` frame carries.
pub const CHUNK: usize = 64 * 1024;
/// The credit each direction of a channel is granted when it opens: the most bytes a sender may have sent on it that
/// the receiver has not yet handed back with `link.credit`.
pub const WINDOW: u32 = 256 * 1024;
/// The most channels one link holds open at once, channel 0 included; the open past it is refused `too_many_channels`.
pub const MAX_CHANNELS: usize = 128;

/// `link.hello` request, the first frame of a link. A refusal is the standard error payload with `code`
/// `link_mismatch` (the router speaks another link number, given in `link`) or `unreachable` (the router could not
/// reach its daemon for channel 0), after which the router ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHelloReq {
    pub link: u32,
}

/// `link.hello` response: the router's link number and its build, for the window's log. Channel 0 is open from here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHelloRes {
    pub link: u32,
    #[serde(default)]
    pub build: String,
}

/// `link.open` request: the window opens channel `ch`, a number it counts up per link and never reuses. The router
/// connects it to its daemon as a connection of its own; the channel's first frame says what it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkOpenReq {
    pub ch: u32,
}

/// `link.open` response. `ok` grants each direction its `WINDOW` of credit. A refusal is `{ch, error, code}` with
/// `code` `unreachable` (the daemon did not take the connection) or `too_many_channels`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkOpenRes {
    pub ch: u32,
    #[serde(default)]
    pub ok: bool,
}

/// `link.open` refusal, and the shape of a refused `link.hello` (with `link` in place of `ch`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRefusal {
    pub error: String,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ch: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<u32>,
}

/// `link.data` event: the blob that follows is the next bytes of channel `ch`, at most `CHUNK`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkDataEvt {
    pub ch: u32,
    pub blob: BlobDescriptor,
}

/// `link.credit` event: the sender of `ch`'s data may send `bytes` more, since the receiver's consumer read them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCreditEvt {
    pub ch: u32,
    pub bytes: u32,
}

/// Why a channel closed. `eof`: its sender is done, after every byte it sent. `reset`: it was dropped, and bytes it
/// held were discarded. `refused`: its far connection could not be made after the open was granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkCloseReason {
    Eof,
    Refused,
    Reset,
}

/// `link.close` event: channel `ch` is closed both ways; nothing more is sent on it, and frames still in flight for it
/// are dropped by the receiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCloseEvt {
    pub ch: u32,
    pub reason: LinkCloseReason,
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod link_tests;
