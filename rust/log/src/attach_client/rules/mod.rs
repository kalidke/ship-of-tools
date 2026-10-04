//! The FE attach-only client's six rulings (ADR 0041 step 6), each a pure
//! state machine with no I/O, so every rule runs under `cargo test` on
//! every platform. The runtime that applies them to a live lane is
//! `client` (generic over `Endpoint`); it is the only caller that also
//! touches the OS.
//!
//! The rulings are lettered as the ADR lists them:
//! (a) [`QuitDispatcher`], (b) [`TakeTransaction`], (c) [`OutstandingSlot`],
//! (d) [`ReconnectState`], (e) [`attach_notice_text`], (f) [`FeDownBaseline`] /
//! [`build_fe_down_marker`].

mod notice;
mod outstanding;
mod quit;
mod reconnect;
mod take;

pub use notice::*;
pub use outstanding::*;
pub use quit::*;
pub use reconnect::*;
pub use take::*;
