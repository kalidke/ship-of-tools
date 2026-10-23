//! The one client for every viewer of a row: the parser and the pane's bookkeeping over an [`AttachWorker`].
//!
//! The worker (`attach`) owns the connection, the attach and the redial; this client owns the screen. [`Self::pump`]
//! drains the worker's events into a vt100 parser without blocking: the snapshot replaces the parser's screen, the
//! output follows in order. The window attaches as the row's driver and reflows every snapshot to its pane; the
//! headless client (the wake and the dialog tick, the `pty.*` ops) attaches as a watcher, adopts the row's own size and
//! takes the pen only to type.

use crate::attach_client::attach::{AttachWorker, HELD_INPUT_CAP};
pub use crate::attach_client::attach::{InputOutcome, WorkerEvent};
use crate::lane::client::Endpoint;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// FE-side scrollback depth for the restored screen: a UI parameter, not the row's.
const SCROLLBACK_ROWS: usize = 5000;
/// How many bytes of output one `pump()` drains before it returns, so a flood is drained in slices across frames; the
/// caller already asks for another redraw whenever `pump()` returns `true`.
const PUMP_DRAIN_CAP_BYTES: usize = 1024 * 1024;
/// The most input one `send_input` carries; the same as a held input's cap, which a headless caller checks before it
/// attaches.
pub const TAKE_QUEUE_CAP: usize = HELD_INPUT_CAP;
/// Prefix of the status `pump()` sets when a snapshot does not restore, which a stale "attached" must not overwrite.
const CHECKPOINT_RESTORE_FAILED_PREFIX: &str = "checkpoint restore failed";

/// What starting a client can fail on; everything after it is reported through [`FeAttachClient::pump`].
#[derive(Debug)]
pub enum FeAttachError {
    SpawnWorkerThread(std::io::Error),
}

impl std::fmt::Display for FeAttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeAttachError::SpawnWorkerThread(e) => write!(f, "spawn fe-client worker thread: {e}"),
        }
    }
}

pub struct FeAttachClient {
    parser: vt100_ctt::Parser,
    /// The pane's `(rows, cols)`, tracked apart from whatever size a restored snapshot carries: a watcher cannot
    /// correct the row's geometry, so every snapshot is reflowed to this size, not the other way round.
    pane_size: (u16, u16),
    worker: AttachWorker,
    events_rx: Receiver<WorkerEvent>,
    status: String,
    dead: bool,
    /// A snapshot has been applied to `parser`, whether or not it restored.
    checkpointed: bool,
    /// A client with no viewport adopts the snapshot's own size instead of reflowing it.
    headless: bool,
}

impl FeAttachClient {
    /// Starts a client on `row` in `endpoint`'s own namespace and returns at once: the worker dials, attaches and
    /// redials on its own. As the row's driver, at `cols`x`rows`.
    pub fn attach<E>(endpoint: E, row: String, cols: u16, rows: u16, wake: Box<dyn Fn() + Send + 'static>) -> Result<Self, FeAttachError>
    where
        E: Endpoint + Send + 'static,
        E::Client: 'static,
    {
        Self::start(endpoint, row, cols, rows, wake, false)
    }

    /// A headless attach: the daemon's own client on a row (the wake, the dialog tick, `pty.input`, `pty.screen`),
    /// never the frontend. A watcher with no viewport: it adopts the row's size and takes the pen only to type, once.
    pub fn attach_headless<E>(endpoint: E, row: String) -> Result<Self, FeAttachError>
    where
        E: Endpoint + Send + 'static,
        E::Client: 'static,
    {
        // Placeholder viewport: the first snapshot replaces it and its size is adopted.
        const PLACEHOLDER_SIZE: u16 = 24;
        Self::start(endpoint, row, PLACEHOLDER_SIZE, PLACEHOLDER_SIZE, Box::new(|| {}), true)
    }

    fn start<E>(endpoint: E, row: String, cols: u16, rows: u16, wake: Box<dyn Fn() + Send + 'static>, headless: bool) -> Result<Self, FeAttachError>
    where
        E: Endpoint + Send + 'static,
        E::Client: 'static,
    {
        let rows = rows.max(2);
        let cols = cols.max(2);
        let (events_tx, events_rx) = mpsc::channel::<WorkerEvent>();
        let sink = move |e: WorkerEvent| {
            let _ = events_tx.send(e);
            wake();
        };
        let worker = AttachWorker::spawn(endpoint, row, cols, rows, headless, sink).map_err(FeAttachError::SpawnWorkerThread)?;
        Ok(Self::with_worker(worker, events_rx, rows, cols, headless))
    }

    fn with_worker(worker: AttachWorker, events_rx: Receiver<WorkerEvent>, rows: u16, cols: u16, headless: bool) -> Self {
        Self {
            parser: vt100_ctt::Parser::new(rows, cols, SCROLLBACK_ROWS),
            pane_size: (rows, cols),
            worker,
            events_rx,
            status: "connecting\u{2026}".to_string(),
            dead: false,
            checkpointed: false,
            headless,
        }
    }

    /// Drains pending events into the parser and the status (non-blocking). `true` when anything changed, so the
    /// caller schedules a repaint. At most [`PUMP_DRAIN_CAP_BYTES`] of output a call.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        let mut drained_output_bytes = 0usize;
        while drained_output_bytes < PUMP_DRAIN_CAP_BYTES {
            match self.events_rx.try_recv() {
                Ok(WorkerEvent::Checkpoint(bytes)) => {
                    let restored = self.parser.restore_screen(&bytes);
                    if let Err(e) = restored {
                        self.status = format!("{CHECKPOINT_RESTORE_FAILED_PREFIX}: {e:?}");
                    } else if self.headless {
                        self.pane_size = self.parser.screen().size();
                    } else {
                        // `restore_screen` replaces the screen with one at the snapshot's own size; reflow it to the
                        // pane now rather than wait for a resize only the driver may send. `set_size` pads and clips
                        // deterministically, so the render never depends on the wire.
                        let (rows, cols) = self.pane_size;
                        self.parser.screen_mut().set_size(rows, cols);
                    }
                    self.checkpointed = true;
                    changed = true;
                }
                Ok(WorkerEvent::Output(bytes)) => {
                    drained_output_bytes += bytes.len();
                    self.worker.ack_output_consumed(bytes.len());
                    self.parser.process(&bytes);
                    changed = true;
                }
                Ok(WorkerEvent::Status(text)) => {
                    // The worker's "attached" follows the snapshot unconditionally; a client that could not render what it
                    // received is not honestly attached, so it must not overwrite the failure.
                    if !(text == "attached" && self.status.starts_with(CHECKPOINT_RESTORE_FAILED_PREFIX)) {
                        self.status = text;
                    }
                    changed = true;
                }
                Ok(WorkerEvent::Terminal(text)) => {
                    tracing::warn!(reason = %text, "fe attach client: reached a terminal state");
                    self.status = text;
                    self.dead = true;
                    changed = true;
                }
                Err(_) => break,
            }
        }
        changed
    }

    pub fn screen(&self) -> &vt100_ctt::Screen {
        self.parser.screen()
    }

    pub fn screen_mut(&mut self) -> &mut vt100_ctt::Screen {
        self.parser.screen_mut()
    }

    pub fn mouse_tracking_on(&self) -> bool {
        !matches!(self.parser.screen().mouse_protocol_mode(), vt100_ctt::MouseProtocolMode::None)
    }

    /// Marks whether this client is the one on screen. A client off screen starts no dial: its worker finishes the
    /// attempt in flight, then waits until it is viewed (ADR 0045 decision 11).
    pub fn set_viewed(&self, viewed: bool) {
        self.worker.set_viewed(viewed);
    }

    /// Inputs dropped for want of room: typed before the attach past what is held, or past the queue's bound.
    pub fn inputs_discarded(&self) -> usize {
        self.worker.inputs_discarded()
    }

    /// Hands keystroke bytes to the worker, which takes the pen first when this client was demoted. Input typed before
    /// the attach is held and sent after it.
    pub fn send_input(&mut self, bytes: &[u8]) {
        if !self.worker.send_input(bytes.to_vec()) {
            tracing::warn!("fe attach client: dropped an input send");
        }
    }

    /// [`Self::send_input`] meant for the run `run` names: it is written there or nowhere, so a client that was reconnected
    /// to a later run since the caller looked at its screen types nothing into it.
    pub fn send_input_in_run(&mut self, bytes: &[u8], run: &str) {
        if !self.worker.send_input_for(bytes.to_vec(), Some(run.to_string())) {
            tracing::warn!("fe attach client: dropped an input send");
        }
    }

    /// Records the pane's size. The local screen is reflowed at once; the row is resized when this client holds the
    /// pen, and at the next attach otherwise.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let rows = rows.max(2);
        let cols = cols.max(2);
        self.pane_size = (rows, cols);
        self.parser.screen_mut().set_size(rows, cols);
        self.worker.resize(cols, rows);
    }

    /// `true` once the episode has ended for good; the status says why.
    pub fn is_dead(&mut self) -> bool {
        self.dead
    }

    pub fn status_line(&self) -> &str {
        &self.status
    }

    /// `true` once a snapshot has been applied, whether or not it restored (a failed one also sets the status). A
    /// watcher's snapshot arrives once per attach, so this never goes back to `false`.
    pub fn is_checkpointed(&self) -> bool {
        self.checkpointed
    }

    /// Bytes the row has written to the terminal for this client, summed.
    pub fn recorded_bytes(&self) -> u64 {
        self.worker.recorded_bytes()
    }

    /// The row's answer to the most recent input.
    pub fn last_input_outcome(&self) -> Option<InputOutcome> {
        self.worker.last_input_outcome()
    }

    /// The run the row declared at the last attach; empty before the first.
    pub fn run(&self) -> String {
        self.worker.run()
    }

    /// Ends the worker (as [`Drop`] does) and waits up to `wait` for its thread: `true` when it ended.
    pub fn shutdown(&mut self, wait: Duration) -> bool {
        self.worker.shutdown(wait)
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
