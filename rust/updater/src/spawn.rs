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
pub(crate) mod tests {
    use super::*;

    pub(crate) struct RecordingSpawner<F>(pub F);
    impl<F> Spawner for RecordingSpawner<F>
    where
        F: Fn(&Command) -> io::Result<Output> + Send + Sync,
    {
        fn output<'a>(
            &'a self,
            command: &'a mut Command,
        ) -> Pin<Box<dyn Future<Output = io::Result<Output>> + Send + 'a>> {
            Box::pin(async move { (self.0)(command) })
        }
    }

    pub(crate) fn output(code: i32, stdout: impl Into<Vec<u8>>) -> Output {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code << 8)
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code as u32)
        };
        Output {
            status,
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }

    pub(crate) fn reject(_: &Command) -> io::Result<Output> {
        panic!("unexpected command in a filesystem-only updater fixture")
    }
}
