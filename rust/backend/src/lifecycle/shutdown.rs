//! shutdown.rs — ending this computer's sessions without resuming any,
//! and the one terminal every controlled daemon exit goes through.
//!
//! The close asks rows to end every row and the drawer by one deadline
//! (`rows::end::end_every`); what rows could not end comes back counted,
//! never guessed. The daemon's own children
//! receive checked termination requests through [`super::child_signal`]:
//! [`exit`] fires its signal before the one raw process exit, and does not
//! wait for any tree to die.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::time::Instant;

use sot_protocol::ops::lease as bounds;

use crate::lifecycle::lease::Leases;
use crate::rows::end::EndReport;
use crate::rows::{WorkspaceChanged, Workspaces};

/// The end-run reason a window's close records.
pub(crate) const REASON: &str = "window closed";

/// `SOT_TEST_SHUTDOWN_BOUND_MS` overrides [`bounds::SHUTDOWN_BOUND`] for
/// tests, read once per process; unset in every real deployment.
pub(crate) fn shutdown_bound() -> Duration {
    static OVERRIDE_MS: OnceLock<Option<u64>> = OnceLock::new();
    let override_ms = *OVERRIDE_MS.get_or_init(|| {
        std::env::var("SOT_TEST_SHUTDOWN_BOUND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    override_ms.map(Duration::from_millis).unwrap_or(bounds::SHUTDOWN_BOUND)
}

/// The shutdown (1.4), after step 1 stopped the accepting; it never
/// returns. Step 0 is a backstop thread that exits 1 at the bound through [`exit`], for
/// the next start to finish; a stalled OS child creation or adoption can delay that exit's
/// fire. Steps 2 and 3 share the rows deadline,
/// `decided + bound - SHUTDOWN_TAIL`; the tail is not scaled with an
/// overridden bound, because steps 4 to 6 take as long either way. Then the final
/// record, the waiting closer's answer, and exit 0 through [`exit`], which fires the child signal.
pub(crate) async fn run(
    leases: Arc<Leases>,
    workspaces: Workspaces,
    ws_events: broadcast::Sender<WorkspaceChanged>,
    decided: Instant,
) -> ! {
    let bound = shutdown_bound();
    let backstop = bound.saturating_sub(decided.elapsed());
    std::thread::spawn(move || {
        std::thread::sleep(backstop);
        tracing::error!("shutdown still running after {bound:?}: exiting 1 after the child fire; the next start finishes it");
        exit(1);
    });
    leases.begin_close();
    tracing::info!("shutting down: ending this computer's sessions");
    #[cfg(feature = "daemon-lifetime-faults")]
    super::test_gates::held("close-go").await;

    let rows_deadline = decided + bound.saturating_sub(bounds::SHUTDOWN_TAIL);
    let gate = workspaces.clone();
    let gate_deadline = std::time::Instant::now() + rows_deadline.saturating_duration_since(Instant::now());
    let settled = tokio::time::timeout_at(
        rows_deadline,
        tokio::task::spawn_blocking(move || gate.close_gate_and_settle(gate_deadline)),
    )
    .await;
    if !matches!(settled, Ok(Ok(true))) {
        tracing::warn!("shutdown: a run start was still in flight at the rows deadline");
    }

    let report = match sot_platform::host::state_dir::sot_state_dir() {
        Some(state_root) => {
            crate::rows::end::end_every(workspaces.list(), &workspaces, &ws_events, &state_root, REASON, rows_deadline).await
        }
        None => {
            // No state root: nothing can be ended, so every row is counted.
            let rows = workspaces.list().len() as u32;
            tracing::warn!(rows, "shutdown: no state root, so no row was ended; all are counted not ended");
            EndReport { not_ended: rows, ..Default::default() }
        }
    };

    if let Err(e) = leases.finish_shutdown(report.not_ended, report.forget.clone()) {
        tracing::error!("shutdown: the final held record was not written: {e}");
    }
    if leases.closer_waits() {
        let wait = bounds::LEASE_REPLY_WAIT * 2 + bounds::NOTICE_ACK_WAIT;
        let _ = tokio::time::timeout(wait, leases.answered()).await;
    }
    tracing::info!(ended = report.ended.len(), not_ended = report.not_ended, "shutdown complete");
    exit(bounds::EXIT_REQUESTED_SHUTDOWN)
}

/// How long the exit waits for the fire to answer before it exits anyway.
const FIRE_WAIT: Duration = Duration::from_secs(2);

/// How long the exit waits, after the fire, for its children's temporary folders to go before it exits anyway.
const FOLDERS_WAIT: Duration = Duration::from_secs(2);

/// Fire the child signal, remove the children's temporary folders, and then take the one raw daemon exit. A controlled
/// exit of the serving daemon (the close, the backstop, an update restart, a handled signal, the main result) comes here,
/// so each contained tree is asked to end first; the first caller's code is the one the process exits with
/// ([`terminal`]). Request errors are logged and the chosen code stands; this is no wait for death, and the exit waits no
/// longer than [`FIRE_WAIT`] for the requests either: a child creation stalled in the OS holds the mutex the fire needs,
/// and the exit, the backstop's included, does not depend on it. It waits no longer than [`FOLDERS_WAIT`] for the
/// folders (`sidecars::child::remove_own`), whose rest the next start removes.
pub(crate) fn exit(code: i32) -> ! {
    terminal(super::child_signal::process(), code, |code| {
        crate::sidecars::child::remove_own(FOLDERS_WAIT);
        std::process::exit(code)
    })
}

/// [`exit`] for a caught INT or TERM: the same terminal (its claim and its fire) and the same removal of the children's
/// folders, then death by that signal with its default action, so whatever started the daemon sees the end it would see
/// with no handler (a service manager counts INT and TERM as a clean stop, a shell reads 128 + n, and the guard mirrors
/// the signal). If the raise returns, the exit is 128 + n.
#[cfg(unix)]
pub(crate) fn exit_by_signal(signum: i32) -> ! {
    terminal(super::child_signal::process(), 128 + signum, |code| {
        crate::sidecars::child::remove_own(FOLDERS_WAIT);
        // SAFETY: the default action restored for one valid signal, the signal unblocked in this thread, then raised at
        // this thread; no handler runs.
        unsafe {
            libc::signal(signum, libc::SIG_DFL);
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, signum);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            libc::raise(signum);
        }
        std::process::exit(code)
    })
}

/// The terminal body: claim the exit, fire `signal` on a thread of its own, wait at most [`FIRE_WAIT`] for it, then hand
/// `code` to `terminate`. Two controlled ends can arrive together (the backstop and a close that finishes at the bound):
/// the first to arrive claims the exit and its code stands; the other waits for the process to end under it. A test
/// supplies a private signal and a callback.
pub(super) fn terminal<T>(
    signal: &'static super::child_signal::Signal,
    code: i32,
    terminate: impl FnOnce(i32) -> T,
) -> T {
    if !signal.claim_exit() {
        loop {
            std::thread::park();
        }
    }
    let (sent, answered) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("sotd-fire".into())
        .spawn(move || {
            let _ = sent.send(signal.fire());
        });
    let fired = match started {
        Ok(_) => answered.recv_timeout(FIRE_WAIT).ok(),
        // No thread to run it on: no fire. Fired here it could wait without bound on a creation stalled in the OS (the
        // registry mutex), and this caller holds the exit's claim, so nothing else could exit either; on Linux the guard
        // ends what the daemon started.
        Err(error) => {
            tracing::error!(%error, "terminal child fire: no thread to run it on; exiting with {code} without it");
            eprintln!("sotd: terminal child fire: no thread to run it on ({error}); exiting with {code} without it");
            return terminate(code);
        }
    };
    match fired {
        Some(Ok(())) => {}
        Some(Err(error)) => {
            tracing::error!(%error, "terminal child fire failed");
            eprintln!("sotd: terminal child fire failed: {error}");
        }
        None => {
            tracing::error!("terminal child fire still running after {FIRE_WAIT:?}: exiting with {code} regardless");
            eprintln!("sotd: terminal child fire still running after {FIRE_WAIT:?}: exiting with {code} regardless");
        }
    }
    terminate(code)
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
