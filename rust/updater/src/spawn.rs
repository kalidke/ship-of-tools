//! Required caller policy for every updater command. This crate supplies no native runner.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::Output;
use tokio::process::Command;

/// Own the command lifetime and capture both output streams. Cancellation initiates termination and transfers
/// an unreaped direct child to the caller runtime's reaper; it cannot promise synchronous async reap from Drop.
/// Tree containment is the caller's policy. Every spawn-bearing updater entry requires this interface.
pub trait Spawner: Send + Sync {
    fn output<'a>(
        &'a self,
        command: &'a mut Command,
    ) -> Pin<Box<dyn Future<Output = io::Result<Output>> + Send + 'a>>;
}

#[cfg(test)]
#[path = "spawn_tests.rs"]
pub(crate) mod tests;
