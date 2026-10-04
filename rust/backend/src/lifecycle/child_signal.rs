//! child_signal.rs — the one process-wide `fired` flag every child owner
//! selects on, because `kill_on_drop` does not run at `process::exit`, and
//! the count of children still alive.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use tokio::sync::watch;

/// One shutdown signal and the count of children still alive under it.
/// The process has one ([`fire`], [`fired`], [`ChildGuard::new`]).
pub(crate) struct Signal {
    fired: watch::Sender<bool>,
    live: AtomicUsize,
}

impl Signal {
    pub(crate) fn new() -> Self {
        Signal { fired: watch::channel(false).0, live: AtomicUsize::new(0) }
    }

    pub(crate) fn fire(&self) {
        self.fired.send_replace(true);
    }

    pub(crate) fn is_fired(&self) -> bool {
        *self.fired.borrow()
    }

    pub(crate) async fn fired(&self) {
        let mut rx = self.fired.subscribe();
        let _ = rx.wait_for(|fired| *fired).await;
    }

    pub(crate) fn guard(&'static self) -> ChildGuard {
        self.live.fetch_add(1, Ordering::SeqCst);
        ChildGuard(&self.live)
    }

    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
}

/// The daemon's one signal; every production owner is given this.
pub(crate) fn process() -> &'static Signal {
    static SIGNAL: OnceLock<Signal> = OnceLock::new();
    SIGNAL.get_or_init(Signal::new)
}

/// Fire the shutdown: every child owner selecting on [`fired`] kills its
/// child. It stays fired for the life of the process.
pub(crate) fn fire() {
    process().fire()
}

/// Resolves once the shutdown has fired; at once if it already has.
pub(crate) async fn fired() {
    process().fired().await
}

/// Children whose [`ChildGuard`] is still alive.
pub(crate) fn live_children() -> usize {
    process().live()
}

/// Counts one child alive until dropped. Its owner keeps it beside the
/// `Child`, so it drops once the child is reaped.
pub(crate) struct ChildGuard(&'static AtomicUsize);

impl ChildGuard {
    pub(crate) fn new() -> Self {
        process().guard()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn child_guard_killed_on_fire() {
        // A private signal: firing the process's own would kill every other
        // test's children.
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut child = tokio::process::Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        let guard = signal.guard();
        assert_eq!(signal.live(), 1);
        let owner = tokio::spawn(async move {
            let _guard = guard;
            tokio::select! {
                _ = child.wait() => {}
                _ = signal.fired() => { let _ = child.kill().await; }
            }
            child.try_wait().ok().flatten()
        });
        signal.fire();
        let status = tokio::time::timeout(Duration::from_secs(3), owner)
            .await
            .expect("the child was not reaped within 3 s")
            .expect("owner task");
        assert!(status.is_some(), "the child was killed but not reaped");
        assert_eq!(signal.live(), 0);
    }
}
