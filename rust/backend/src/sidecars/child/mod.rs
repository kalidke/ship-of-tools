//! What each child the daemon starts carries with it.

mod stderr_tail;
mod tmp;

pub(crate) use stderr_tail::{last_lines, Start, StderrTail};
pub(crate) use tmp::{remove_own, sweep_gone, ChildTmp};
