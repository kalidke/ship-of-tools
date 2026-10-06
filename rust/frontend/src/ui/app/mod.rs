//! The winit application: App holds the window's State and its startup inputs; frame pacing.

use super::*;

mod exit;
mod frame;
mod handler;
#[cfg(test)]
mod exit_process_tests;

pub(in crate::ui) use exit::*;

pub struct App {
    state: Option<State>,
    /// Inputs forwarded into State::new the first time the event loop hands
    /// us a window. Held on App rather than constructed inside resumed() so
    /// main.rs can decide whether transport runs.
    evt_rx:
        Option<std::sync::mpsc::Receiver<(crate::net::dial::HostKey, crate::net::transport::IncomingEvt)>>,
    rt: Option<tokio::runtime::Runtime>,
    cli: crate::cli::Cli,
    /// Fans in every host's transport task; each clones this once in
    /// `resumed()` before it's handed off (see `PendingTransport`).
    evt_tx: Option<std::sync::mpsc::Sender<(crate::net::dial::HostKey, crate::net::transport::IncomingEvt)>>,
    /// One outgoing-request sender per host, in connection (display) order
    /// — the send-side half of `PendingTransport`. GPU thread holds these;
    /// `resumed()` spawns the matching transport task for each.
    conns: Vec<(
        crate::net::dial::HostKey,
        tokio::sync::mpsc::UnboundedSender<crate::net::transport::OutgoingReq>,
    )>,
    pending_transports: Option<Vec<PendingTransport>>,
    leases: Arc<crate::lease::Leases>,
    /// Tracks Ctrl/Shift/Alt/Super state for Ctrl+Arrow pane navigation.
    /// winit 0.30 publishes modifier changes via `WindowEvent::ModifiersChanged`
    /// separately from key presses, so we keep a running copy and consult
    /// it inside the KeyboardInput arm.
    modifiers: winit::keyboard::ModifiersState,
}

impl App {
    pub fn new(
        evt_rx: std::sync::mpsc::Receiver<(crate::net::dial::HostKey, crate::net::transport::IncomingEvt)>,
        rt: Option<tokio::runtime::Runtime>,
        cli: crate::cli::Cli,
        evt_tx: std::sync::mpsc::Sender<(crate::net::dial::HostKey, crate::net::transport::IncomingEvt)>,
        conns: Vec<(
            crate::net::dial::HostKey,
            tokio::sync::mpsc::UnboundedSender<crate::net::transport::OutgoingReq>,
        )>,
        pending_transports: Option<Vec<PendingTransport>>,
        leases: Arc<crate::lease::Leases>,
    ) -> Self {
        Self {
            state: None,
            evt_rx: Some(evt_rx),
            rt,
            cli,
            evt_tx: Some(evt_tx),
            conns,
            pending_transports,
            leases,
            modifiers: winit::keyboard::ModifiersState::empty(),
        }
    }

    /// Own the returning event loop and finalize transport before any App field drops.
    pub fn run(mut self, event_loop: winit::event_loop::EventLoop<()>) -> Result<(), winit::error::EventLoopError> {
        self.run_with(|app| event_loop.run_app(app))
    }

    fn run_with(&mut self, run: impl FnOnce(&mut Self) -> Result<(), winit::error::EventLoopError>) -> Result<(), winit::error::EventLoopError> {
        let result = run(self);
        self.shutdown_transport();
        result
    }

    fn shutdown_transport(&mut self) {
        if let Some(runtime) = self.rt.take() {
            drop(runtime);
        }
    }

}

/// Minimum time between frames in interactive mode (~120 fps). Picks the
/// tightest cap that still gives a paste burst, a PTY echo storm, and the
/// keystroke that fired them room to coalesce into one frame, since the
/// monitor can't display faster than its refresh rate anyway.
const FRAME_BUDGET: std::time::Duration = std::time::Duration::from_micros(8_333);

/// Settle delay for cursor-driven backend round-trips (`preview.get`,
/// `concept.read`, `file.parse` drift check, `tmux.capture_pane`). Without
/// this gate, hold-to-scroll generates one round-trip per visited row —
/// hundreds per second for fast scroll — which saturates the SSH tunnel
/// and renders many heavy preview blobs through wgpu in rapid succession.
/// Symptom: transport reconnect (which then re-fires `tree.root` and
/// resets the cursor to row 0) + GPU pressure (AMD driver overlay fires).
/// 150ms is short enough to feel instant on settle, long enough to absorb
/// any realistic auto-repeat rate.
pub(in crate::ui) const NAV_FIRE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);
