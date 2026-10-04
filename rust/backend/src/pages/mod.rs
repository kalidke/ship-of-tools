//! Loopback HTTP pages and the proxy that reaches them (the video server and
//! `proxy.connect`). Part of the daemon's page serving; see CLAUDE.md here.

mod http;
pub(super) mod proxy;
pub(super) mod video;
