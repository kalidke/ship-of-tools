//! retire.rs — in-process services that retire before the daemon's terminal.
//!
//! A service that runs inside `sotd` and owns a network listener (the project chat is the first) registers a
//! [`Participant`]. A controlled end of the serving daemon hands the registry to [`retire`] from the terminal
//! (`shutdown::terminal`), which closes every participant's admission, then asks each to stop, then waits for each to
//! report itself retired, all by one bound. A participant that does not finish is named in the [`Retirement`] and
//! logged; it never changes the exit code and never extends the end. With nothing registered the terminal is
//! untouched.

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// How long the terminal waits for retirement, charged to [`super::shutdown::FIRE_WAIT`]: the fire's wait is what is
/// left of that budget, so the terminal's total stays `FIRE_WAIT` + `FOLDERS_WAIT`.
pub(super) const RETIRE_WAIT: Duration = Duration::from_secs(1);

/// An in-process service the daemon's end waits for. The three steps are called in this order, each participant's
/// closure before any participant's stop, from one thread the terminal bounds; none of them may rely on the runtime
/// that is ending.
pub(crate) trait Participant: Send + Sync {
    /// A name for the log and the report.
    fn name(&self) -> &'static str;
    /// Stop admitting new work. Called first, for every participant, and quick.
    fn close_admission(&self);
    /// Ask the participant to finish or give up. Does not wait.
    fn request_stop(&self);
    /// Wait up to `within` for the participant to be gone; true when it is.
    fn wait_retired(&self, within: Duration) -> bool;
}

/// What retirement did: the names of the participants that were not confirmed retired within the bound.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Retirement {
    pub(crate) incomplete: Vec<&'static str>,
}

/// The registered participants; `None` once retirement has begun, so a late registrant is refused, never missed.
pub(crate) struct Registry {
    participants: Mutex<Option<Vec<Arc<dyn Participant>>>>,
}

impl Registry {
    pub(crate) const fn new() -> Self {
        Registry { participants: Mutex::new(Some(Vec::new())) }
    }

    /// Register `participant`; false when retirement has already begun and the caller must not start.
    #[cfg_attr(not(test), allow(dead_code, reason = "the project chat is the first caller"))]
    pub(crate) fn register(&self, participant: Arc<dyn Participant>) -> bool {
        match self.participants.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            Some(list) => {
                list.push(participant);
                true
            }
            None => false,
        }
    }

    fn take(&self) -> Vec<Arc<dyn Participant>> {
        self.participants.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default()
    }
}

/// The daemon's registry, the one the terminal retires.
pub(crate) fn participants() -> &'static Registry {
    static REGISTRY: Registry = Registry::new();
    &REGISTRY
}

/// Retire every participant of `registry` within `bound`: close admission for all, request stop of all, then wait for
/// each. The steps run on a thread of their own, so a participant that blocks in any of them costs the bound and no
/// more; those not confirmed by then are the report's `incomplete`. A second call finds the registry empty.
pub(super) fn retire(registry: &Registry, bound: Duration) -> Retirement {
    let list = registry.take();
    if list.is_empty() {
        return Retirement::default();
    }
    let names: Vec<&'static str> = list.iter().map(|p| p.name()).collect();
    let (done, finished) = mpsc::channel();
    let deadline = std::time::Instant::now() + bound;
    let started = std::thread::Builder::new().name("sotd-retire".into()).spawn(move || {
        for p in &list {
            p.close_admission();
        }
        for p in &list {
            p.request_stop();
        }
        for (i, p) in list.iter().enumerate() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if p.wait_retired(left) {
                let _ = done.send(i);
            }
        }
    });
    if started.is_err() {
        tracing::error!("retirement: no thread to run it on; no participant was asked to stop");
        return Retirement { incomplete: names };
    }
    let mut retired = vec![false; names.len()];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match finished.recv_timeout(left) {
            Ok(i) => retired[i] = true,
            Err(_) => break,
        }
    }
    let incomplete: Vec<&'static str> =
        names.iter().zip(&retired).filter(|(_, r)| !**r).map(|(n, _)| *n).collect();
    if !incomplete.is_empty() {
        tracing::error!(?incomplete, "retirement incomplete after {bound:?}: exiting regardless");
        eprintln!("sotd: retirement incomplete after {bound:?} ({}): exiting regardless", incomplete.join(", "));
    }
    Retirement { incomplete }
}

#[cfg(test)]
#[path = "retire_tests.rs"]
mod tests;
