//! Everything written to the terminal, off the run's main loop.
//!
//! A terminal's input buffer is small and an agent may read nothing from it until its own output drains, so a write to
//! it can block for as long as the agent likes. Done on the loop's thread that would stop the loop draining output, the
//! agent would then block writing output, and the two would wait on each other for good. So the loop writes nothing to
//! the terminal itself: typed input and the host's own replies (the DA1 answer) are handed to one thread, which
//! writes them in the order they were handed over, each typed write answered when its bytes are in the terminal.
//! The run retires the thread at its end: what is still waiting is answered `run ended`, and the thread is joined.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Typed writes the pump has not yet started, at most; one more is refused as busy. The host's own replies may use as
/// many again.
const PENDING_WRITES: usize = 16;

/// What a write is answered when the run is over.
const RUN_OVER: &str = "run ended: the run is over and the terminal's input is closed";

type Reply = mpsc::SyncSender<Result<usize, String>>;

struct Job {
    bytes: Vec<u8>,
    reply: Option<Reply>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

pub struct InputPump {
    shared: Arc<Shared>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl InputPump {
    /// Starts the thread that writes to `terminal`. It ends when the pump is retired, or when a write fails because the
    /// terminal is gone.
    pub fn new(mut terminal: impl Write + Send + 'static) -> std::io::Result<Self> {
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), ready: Condvar::new() });
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new().name("row-input".into()).spawn(move || loop {
            let job = {
                let mut state = worker.lock();
                loop {
                    if state.closed {
                        return;
                    }
                    if let Some(job) = state.queue.pop_front() {
                        break job;
                    }
                    state = worker.ready.wait(state).unwrap_or_else(|p| p.into_inner());
                }
            };
            let written = terminal.write_all(&job.bytes).map(|()| job.bytes.len()).map_err(|e| e.to_string());
            let failed = written.is_err();
            if let Some(reply) = job.reply {
                let _ = reply.send(written);
            }
            if failed {
                // The terminal is gone: nothing after this can be written, and the run's end answers what waits.
                return;
            }
        })?;
        Ok(Self { shared, thread: Mutex::new(Some(thread)) })
    }

    /// Hands `bytes` over to be written; `reply` gets the count when they are in the terminal. A pump that already
    /// holds [`PENDING_WRITES`] answers `busy` at once, and a retired one `run ended`.
    pub fn write(&self, bytes: Vec<u8>, reply: Reply) {
        let mut state = self.shared.lock();
        let refusal = if state.closed {
            Some(RUN_OVER.to_string())
        } else if state.queue.len() >= PENDING_WRITES {
            Some("busy: the terminal has not taken the input before this".to_string())
        } else {
            None
        };
        match refusal {
            Some(text) => {
                drop(state);
                let _ = reply.send(Err(text));
            }
            None => {
                state.queue.push_back(Job { bytes, reply: Some(reply) });
                self.shared.ready.notify_one();
            }
        }
    }

    /// Writes `bytes` in order with the typed input, answering nobody: the host's own replies on the terminal. `false`
    /// when the pump is retired or too far behind to take them.
    pub fn write_detached(&self, bytes: Vec<u8>) -> bool {
        let mut state = self.shared.lock();
        if state.closed || state.queue.len() >= PENDING_WRITES * 2 {
            return false;
        }
        state.queue.push_back(Job { bytes, reply: None });
        self.shared.ready.notify_one();
        true
    }

    /// The run is over: pending writes are answered `run ended` and never written, and the thread is joined within
    /// `bound` (a write blocked in the terminal returns once the agent's terminal is hung up). `true` when it ended.
    pub fn retire(&self, bound: Duration) -> bool {
        let pending: Vec<Job> = {
            let mut state = self.shared.lock();
            state.closed = true;
            self.shared.ready.notify_all();
            state.queue.drain(..).collect()
        };
        for job in pending {
            if let Some(reply) = job.reply {
                let _ = reply.send(Err(RUN_OVER.to_string()));
            }
        }
        let Some(thread) = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take() else { return true };
        let deadline = Instant::now() + bound;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = thread.join();
        true
    }
}

impl Drop for InputPump {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.closed = true;
        self.shared.ready.notify_all();
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
