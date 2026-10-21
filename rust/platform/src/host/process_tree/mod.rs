//! The platform's native process-birth primitives: the part of "a process starts inside an owner's reach" that is
//! an operating-system mechanism and not a policy. On Unix that is the two-phase launcher (`native_birth.c`, wrapped
//! by `unix_birth.rs`): a fork whose child waits at a gate, returned to its caller before the target runs.

#[cfg(unix)]
mod unix_birth;

#[cfg(unix)]
pub use unix_birth::{Birth, BirthError, Group, Launch, Ready, Stage, GATE_GO_BYTE};
#[cfg(all(unix, feature = "native-barrier"))]
pub use unix_birth::{Pause, PAUSE_BEFORE_CLOSE, PAUSE_BEFORE_READY, PAUSE_BEFORE_SESSION};
