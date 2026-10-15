//! What each child the daemon starts carries with it.

mod stderr_tail;
mod tmp;

pub(crate) use stderr_tail::StderrTail;
pub(crate) use tmp::ChildTmp;
