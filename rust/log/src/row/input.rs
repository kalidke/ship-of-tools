//! Typed input on its way to the terminal, off the run's main loop.
//!
//! A terminal's input buffer is small and an agent may read nothing from it until its own output drains, so a write to
//! it can block for as long as the agent likes. Done on the loop's thread that would stop the loop draining output, the
//! agent would then block writing output, and the two would wait on each other for good. The writes happen on one
//! thread of their own, in the order they were handed over, each answered when its bytes are in the terminal.

use std::io::Write;
use std::sync::mpsc;

/// Writes the pump has not yet started, at most; one more is refused as busy.
const PENDING_WRITES: usize = 16;

struct Job {
    bytes: Vec<u8>,
    reply: mpsc::SyncSender<Result<usize, String>>,
}

pub struct InputPump {
    jobs: mpsc::SyncSender<Job>,
}

impl InputPump {
    /// Starts the thread that writes to `terminal`. It ends when the pump is dropped, or when a write fails because the
    /// terminal is gone.
    pub fn new(mut terminal: impl Write + Send + 'static) -> std::io::Result<Self> {
        let (jobs, inbox) = mpsc::sync_channel::<Job>(PENDING_WRITES);
        std::thread::Builder::new().name("row-input".into()).spawn(move || {
            for job in inbox {
                let written = terminal.write_all(&job.bytes).map(|()| job.bytes.len()).map_err(|e| e.to_string());
                let _ = job.reply.send(written);
            }
        })?;
        Ok(Self { jobs })
    }

    /// Hands `bytes` over to be written; `reply` gets the count when they are in the terminal. A pump that already
    /// holds [`PENDING_WRITES`] answers `busy` at once.
    pub fn write(&self, bytes: Vec<u8>, reply: mpsc::SyncSender<Result<usize, String>>) {
        match self.jobs.try_send(Job { bytes, reply }) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(job)) => {
                let _ = job.reply.send(Err("busy: the terminal has not taken the input before this".into()));
            }
            Err(mpsc::TrySendError::Disconnected(job)) => {
                let _ = job.reply.send(Err("the terminal's input is closed".into()));
            }
        }
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
