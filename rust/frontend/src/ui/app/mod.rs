//! The winit application: App holds the window's State and its startup inputs; frame pacing.

use super::*;

mod exit;
mod frame;
mod handler;
#[cfg(test)]
#[cfg_attr(feature = "test-window-progress", allow(unused_imports, reason = "the harness=false native targets run none of these tests"))]
mod exit_process_tests;

#[cfg(all(test, feature = "test-window-progress"))]
pub(super) mod tests;
#[cfg(all(test, feature = "test-window-progress"))]
pub(super) mod native_children;
#[cfg(all(test, feature = "test-window-close"))]
pub(super) mod native_exit_tests;
#[cfg(all(test, feature = "test-window-close", windows))]
mod native_eventlog;
#[cfg(all(test, feature = "test-pane-timing"))]
pub(super) mod native_pane_tests;
#[cfg(all(test, feature = "test-pane-timing"))]
mod native_pane_daemon_tests;
#[cfg(all(test, feature = "test-pane-timing"))]
mod native_pane_route_tests;

pub(in crate::ui) use exit::*;

use std::sync::atomic::{AtomicBool, Ordering::AcqRel};

/// The one way a thread other than the window's asks it to redraw. On macOS winit runs every `Window` call made off
/// the main thread on the main thread and waits there, so a transport that redrew after each daemon frame stood still
/// for every frame being drawn, and every daemon with it. A sender sets `pending` and, only when it was clear, posts
/// one user event; the window clears it and requests its own redraw (`App::user_event`). At most one event is
/// outstanding however many frames arrive, and a sender queues its work before it wakes, so none is left undrained.
#[derive(Clone)]
pub(crate) struct MainWake {
    pending: Arc<AtomicBool>,
    post: Arc<dyn Fn() + Send + Sync>,
}

impl MainWake {
    pub(crate) fn new(proxy: winit::event_loop::EventLoopProxy<()>) -> Self {
        // After the event loop has ended there is nothing left to wake.
        Self::posting(move || {
            let _ = proxy.send_event(());
        })
    }

    fn posting(post: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            pending: Arc::new(AtomicBool::new(false)),
            post: Arc::new(post),
        }
    }

    /// From any thread: ask the window for a redraw. Never waits on the window's thread.
    pub(crate) fn wake(&self) {
        if !self.pending.swap(true, AcqRel) {
            (self.post)();
        }
    }

    /// On the window's thread: whether a wake was pending, clearing it.
    fn take(&self) -> bool {
        self.pending.swap(false, AcqRel)
    }
}

/// The window as State holds it. The marker makes it `!Send`, so no clone of it reaches another thread or task, where
/// a winit call would wait on the window's thread on macOS; such a thread holds a `MainWake` instead.
#[derive(Clone)]
pub(in crate::ui) struct MainWindow(Arc<Window>, std::marker::PhantomData<*const ()>);

impl MainWindow {
    pub(in crate::ui) fn new(window: Arc<Window>) -> Self {
        Self(window, std::marker::PhantomData)
    }
}

impl std::ops::Deref for MainWindow {
    type Target = Window;
    fn deref(&self) -> &Window {
        &self.0
    }
}

// The class guard, checked by every build on every OS: if `MainWindow` were `Send`, both impls below would apply and
// the call would be ambiguous, so the crate would not compile. `thread::spawn` and `tokio::spawn` require `Send`.
const _: fn() = || {
    trait AmbiguousIfSend<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    let _ = <MainWindow as AmbiguousIfSend<_>>::check;
};

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
    /// Shared with State: the one three-second process backstop.
    deadline: ExitDeadline,
    /// The idle repaint's armed deadline (`about_to_wait`), kept so a wake does not move it.
    idle_due: Option<std::time::Instant>,
    /// Handed to every thread that wakes the window; cleared in `user_event`.
    wake: MainWake,
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
        wake: MainWake,
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
            deadline: ExitDeadline::new(),
            idle_due: None,
            wake,
        }
    }

    /// Own the returning event loop and finalize transport before any App field drops.
    pub fn run(mut self, event_loop: winit::event_loop::EventLoop<()>) -> Result<(), winit::error::EventLoopError> {
        self.run_with(|app| event_loop.run_app(app))
    }

    fn run_with(&mut self, run: impl FnOnce(&mut Self) -> Result<(), winit::error::EventLoopError>) -> Result<(), winit::error::EventLoopError> {
        let result = run(self);
        #[cfg(test)]
        barrier("returned");
        // A return no terminal decision armed (a startup failure, a loop error) gets the fallback.
        self.deadline.arm(if result.is_ok() { 0 } else { 1 });
        #[cfg(test)]
        barrier("fallback");
        self.shutdown_transport();
        result
    }

    fn shutdown_transport(&mut self) {
        if let Some(runtime) = self.rt.take() {
            runtime.shutdown_timeout(crate::lease::LEAVE_WRITE_WAIT);
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

#[cfg(test)]
mod wake_tests {
    /// Wakes before a drain post one event; a wake after the drain posts again. Four threads then queue work and wake
    /// while a loop that drains only when an event arrives drains every item: a wake lost between a queued item and
    /// its drain leaves the loop waiting, and the 10 s bound fails it.
    #[test]
    fn main_wake_coalesces_and_never_loses_a_wake() {
        use super::MainWake;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let posts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&posts);
        let wake = MainWake::posting(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        for _ in 0..5 {
            wake.wake();
        }
        assert_eq!(
            posts.load(Ordering::SeqCst),
            1,
            "five wakes before a drain post one event"
        );
        assert!(wake.take());
        assert!(!wake.take(), "the drain cleared the wake");
        wake.wake();
        assert_eq!(
            posts.load(Ordering::SeqCst),
            2,
            "a wake after the drain posts again"
        );

        let (posted_tx, posted_rx) = std::sync::mpsc::channel::<()>();
        let posted_tx = std::sync::Mutex::new(posted_tx);
        let wake = MainWake::posting(move || {
            let _ = posted_tx.lock().unwrap().send(());
        });
        let (work_tx, work_rx) = std::sync::mpsc::channel::<u32>();
        const SENDERS: u32 = 4;
        const EACH: u32 = 10_000;
        let senders: Vec<_> = (0..SENDERS)
            .map(|_| {
                let (wake, work_tx) = (wake.clone(), work_tx.clone());
                std::thread::spawn(move || {
                    for i in 0..EACH {
                        work_tx.send(i).unwrap();
                        wake.wake();
                    }
                })
            })
            .collect();
        drop(work_tx);
        let mut drained = 0;
        while drained < SENDERS * EACH {
            posted_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap_or_else(|_| {
                    panic!("{drained} items drained, and the rest have no wake pending")
                });
            assert!(wake.take(), "each event stands for one pending wake");
            while work_rx.try_recv().is_ok() {
                drained += 1;
            }
        }
        for sender in senders {
            sender.join().unwrap();
        }
        println!("main-wake: {drained} items from {SENDERS} threads drained");
    }
}
