//! The window's one-login link: one ssh login (or one connection to the hub's relay socket) per window and dialled
//! computer, carrying the control connection, every page connection and every row the window attaches as channels
//! inside wire frames (`sot_protocol::ops::link`). The window end is [`Link`] and the [`LinkHandle`] its callers open
//! channels through; the far end is [`router::serve`], which `sotd stdio-bridge` runs when its first frame is
//! `link.hello`, connecting each channel to its own daemon as a connection of its own.
//!
//! One engine serves both ends: the channel table and its credit rules under one lock (`table.rs`), the link's one
//! reader and one writer (`io.rs`), and a channel's async and blocking faces over the same queues (`channel.rs`).

mod channel;
mod io;
mod local;
pub mod router;
mod table;
mod window;

pub use channel::{Channel, ChannelReader, ChannelWriter};
pub use local::{adopt, connect_local, LocalStream};
pub use window::{Link, LinkHandle, LinkRoute, LinkStartError, OpenError};

/// How long a lane dial over a channel waits for the channel and its handoff replies: no login is inside it, only the
/// link's round trips and the far daemon's start of a row.
pub const LINK_REPLY_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// How long either end of a link waits for a byte from the other before it ends the link: the server's and fe-net's
/// rule. Channel 0's 30 s ping keeps a live link inside it, so the link has no ping of its own.
pub const SILENCE_BOUND: std::time::Duration = std::time::Duration::from_secs(90);

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
