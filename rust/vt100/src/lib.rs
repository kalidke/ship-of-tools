//! This crate parses a terminal byte stream and provides an in-memory
//! representation of the rendered contents.
//!
//! # Overview
//!
//! This is essentially the terminal parser component of a graphical terminal
//! emulator pulled out into a separate crate. Although you can use this crate
//! to build a graphical terminal emulator, it also contains functionality
//! necessary for implementing terminal applications that want to run other
//! terminal applications - programs like `screen` or `tmux` for example.
//!
//! # Synopsis
//!
//! ```
//! let mut parser = vt100_ctt::Parser::new(24, 80, 0);
//!
//! parser.process(b"this text is \x1b[31mRED\x1b[m");
//! assert_eq!(
//!     parser.screen().cell(0, 13).unwrap().fgcolor(),
//!     vt100_ctt::Color::Idx(1),
//! );
//! assert!(parser.screen().contents().starts_with("this text is RED"));
//!
//! // The parsed state can be handed to another process exactly, including
//! // the grid that is not currently on screen (see `Screen::checkpoint`).
//! let bytes = parser.screen().checkpoint().unwrap();
//! let mut elsewhere = vt100_ctt::Parser::default();
//! elsewhere.restore_screen(&bytes).unwrap();
//! assert_eq!(elsewhere.screen().contents(), parser.screen().contents());
//! ```
//!
//! This fork does not reconstruct terminal state as escape sequences.
//! Consumers render from the parsed grid, or move it with
//! [`Screen::checkpoint`] and [`Parser::restore_screen`].

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![warn(clippy::as_conversions)]
#![warn(clippy::get_unwrap)]
#![allow(clippy::cognitive_complexity)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::similar_names)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::type_complexity)]

mod attrs;
mod callbacks;
mod cell;
mod checkpoint;
mod grid;
mod parser;
mod perform;
mod row;
mod screen;

pub use attrs::Color;
pub use callbacks::Callbacks;
pub use checkpoint::{CheckpointError, MAX_CHECKPOINT_LEN};
pub use cell::Cell;
pub use parser::Parser;
pub use screen::{MouseProtocolEncoding, MouseProtocolMode, Screen};
