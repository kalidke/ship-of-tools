//! The state of this daemon's update, in one owner (distribution's contract, under Update).
//!
//! It keeps two facts. The chase is what this process's updater is doing about the newest release: memory only, written
//! at each step of the check and of the stage-prepare-arm pipeline in `update.rs`, and rebuilt by the next check after a
//! restart. The armed fact is what is on disk: the pending pointer, read through the judge `sotd update pointer` answers
//! with (`cli::judge`), so ready is said exactly where the verb answers ok for a pointer that is there. That is not yet
//! the apply: the apply's own checks come after the verb's (the stage's files and digests, the checkout, a pointer for
//! the tag already installed) and are the apply scripts' alone, so a ready update can still be dropped at its start,
//! after which the next check prepares it again. The state told is a pure function of the two facts ([`told`]). Nothing
//! watches the folder: the armed fact is read again at [`Status::reconcile`], which the daemon's start, a pipeline's
//! end, each daily check and each `version.query` call.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sot_protocol::{UpdatePhase, UpdateState};
use tokio::sync::watch;

/// How long a caller waits for the armed fact to be read from disk. Shorter than the shortest wait of a caller of
/// `version.query` (3 s, `sotd status` and the installer), so a disk that is slow reads as a state marked unread, never
/// as a daemon that does not answer.
pub(crate) const RECONCILE_BOUND: Duration = Duration::from_secs(1);

/// What this process's updater is doing about the newest release. A version is bare, with no leading `v`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Chase {
    /// Checks are on and none has run in this process.
    Unchecked,
    /// The last check found nothing above the install.
    None,
    /// A check found `version`; `detail` says why nothing is staging it, when nothing can.
    Found { version: String, detail: String },
    /// This process began the stage of `version` and its arm has not answered.
    Preparing { version: String },
    /// `version` was found and not made ready. `block` is the cause alone when the step that failed was the stage,
    /// which `update.check` reports for that release and no other.
    NotApplied { version: String, cause: String, block: Option<String> },
    /// This process armed `version`: the disk says the rest. It claims no result of its own, so when the pointer is
    /// gone (dropped or applied by another process) the state is a release found, never up to date.
    Armed { version: String },
    Off { why: String },
    Blocked { why: String },
    CheckFailed { why: String },
}

/// What is armed on disk for this install, as the pointer's judge read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Armed {
    Nothing,
    /// A pointer the verb passes (the start's own checks can still drop it), and what applies it.
    Fits { version: String, applies: String },
    /// A pointer the apply would drop, with the judge's words; no version when the pointer cannot be read.
    Refused { version: Option<String>, why: String },
}

/// Why a release this process armed reads as found again.
const ARMED_AND_GONE: &str = "it was armed here and the armed update is no longer on disk; the next check looks again";

/// The state told for a chase and an armed fact. A pointer that fits is ready, unless this process is chasing another
/// release (found, staging, or not made ready), which then comes first with the pointer's version beside it in
/// `armed`; a release whose own pointer fits is ready in every phase of its chase, so the daily check of an armed
/// release changes nothing. Else a pointer the apply would refuse is not applied, unless a release is being chased;
/// else the chase's own phase.
pub(crate) fn told(chase: &Chase, armed: &Armed, checked_s: Option<u64>) -> UpdateState {
    use UpdatePhase as P;
    let (phase, version, detail) = match chase {
        Chase::Unchecked => (P::Unchecked, "", ""),
        Chase::None => (P::None, "", ""),
        Chase::Found { version, detail } => (P::Found, version.as_str(), detail.as_str()),
        Chase::Preparing { version } => (P::Preparing, version.as_str(), ""),
        Chase::NotApplied { version, cause, .. } => (P::NotApplied, version.as_str(), cause.as_str()),
        Chase::Armed { version } => (P::Found, version.as_str(), ARMED_AND_GONE),
        Chase::Off { why } => (P::Off, "", why.as_str()),
        Chase::Blocked { why } => (P::Blocked, "", why.as_str()),
        Chase::CheckFailed { why } => (P::CheckFailed, "", why.as_str()),
    };
    let state = |phase, version: &str, detail: &str, armed: &str| UpdateState { phase, version: version.into(), detail: detail.into(), armed: armed.into(), checked_s, unread: false };
    let chasing = matches!(chase, Chase::Found { .. } | Chase::Preparing { .. } | Chase::NotApplied { .. });
    match armed {
        Armed::Fits { version: fits, applies } if !chasing || fits == version => state(P::Ready, fits, applies, ""),
        Armed::Fits { version: fits, .. } => state(phase, version, detail, fits),
        Armed::Refused { version: refused, why } if !chasing => state(P::NotApplied, refused.as_deref().unwrap_or(""), why, ""),
        Armed::Refused { .. } | Armed::Nothing => state(phase, version, detail, ""),
    }
}

/// What applies an armed update, in the words `ready` carries. They are read in a table of other computers
/// (`sotd status`) as well as on the daemon's own, and on a computer that runs a daemon and no window, so they name
/// the daemon's computer from any reader's seat.
pub(crate) fn applies(auto: bool, unit: bool) -> &'static str {
    match (auto, unit) {
        (true, _) => "applies at that daemon's daily check with no client attached, or at its next start",
        (false, true) => "applies when that computer's sotd.service next starts (a start of Ship of Tools there, or a restart of the service)",
        (false, false) => "applies when Ship of Tools is next started on that computer",
    }
}

/// Where this daemon's armed pointer is, and whether a unit restarts the daemon.
pub(crate) struct Disk {
    pub(crate) updates: PathBuf,
    pub(crate) target: String,
    pub(crate) unit: bool,
}

struct Facts {
    chase: Chase,
    armed: Armed,
    /// When the last check ran.
    checked: Option<Instant>,
}

struct Inner {
    facts: watch::Sender<Facts>,
    /// Update mode `auto`: an armed update applies at a check with no client attached.
    auto: bool,
    /// Asked at each reconcile; `None` where no updates folder resolves or the platform has no release.
    disk: Box<dyn Fn() -> Option<Disk> + Send + Sync>,
    /// The reading of the disk in flight. One at a time: a reading its caller stopped waiting for stays here until it
    /// ends, and no other is begun beside it.
    reading: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// The owner: one per daemon, cloned to whoever writes or reads it.
#[derive(Clone)]
pub(crate) struct Status(Arc<Inner>);

impl Status {
    pub(crate) fn new(start: Chase, auto: bool, disk: impl Fn() -> Option<Disk> + Send + Sync + 'static) -> Self {
        let (facts, _) = watch::channel(Facts { chase: start, armed: Armed::Nothing, checked: None });
        Self(Arc::new(Inner { facts, auto, disk: Box::new(disk), reading: tokio::sync::Mutex::new(None) }))
    }

    /// The state as told now, read from memory.
    pub(crate) fn state(&self) -> UpdateState {
        let facts = self.0.facts.borrow();
        told(&facts.chase, &facts.armed, facts.checked.map(|at| at.elapsed().as_secs()))
    }

    /// The cause of a stage of `version` that failed and has not been tried again, for `update.check`'s status. A
    /// failed stage marks only its own release.
    pub(crate) fn stage_block(&self, version: &str) -> Option<String> {
        match &self.0.facts.borrow().chase {
            Chase::NotApplied { version: failed, block, .. } if failed == version => block.clone(),
            _ => None,
        }
    }

    /// A step of the pipeline: the chase is `chase` from now.
    pub(crate) fn set_chase(&self, chase: Chase) {
        self.write(|facts| facts.chase = chase);
    }

    /// A check ended now with `chase`.
    pub(crate) fn checked(&self, chase: Chase) {
        self.write(|facts| (facts.chase, facts.checked) = (chase, Some(Instant::now())));
    }

    /// Reads the armed fact from disk, and answers whether a reading begun after this call ended within
    /// [`RECONCILE_BOUND`]. When it did not, the last fact stands, the caller goes on, and the reading goes on alone:
    /// it writes its fact when it ends.
    pub(crate) async fn reconcile(&self) -> bool {
        let read = tokio::time::timeout(RECONCILE_BOUND, self.read_disk()).await.is_ok();
        if !read {
            tracing::warn!("the armed update was not read from disk within {RECONCILE_BOUND:?}; the state keeps what was last read");
        }
        read
    }

    /// One reading, after any still in flight. Every read of it (the install record and `hosts.toml` through std, the
    /// pointer and the prepare's record through tokio) runs on a thread of the blocking pool, so a read that hangs
    /// holds that thread and the turn, never a runtime worker or a caller past its bound.
    async fn read_disk(&self) {
        let mut reading = self.0.reading.lock().await;
        if let Some(earlier) = reading.as_mut() {
            let _ = earlier.await;
        }
        let (inner, runtime) = (self.0.clone(), tokio::runtime::Handle::current());
        let mine = reading.insert(tokio::task::spawn_blocking(move || {
            let armed = (inner.disk)().map_or(Armed::Nothing, |disk| {
                let judged = runtime.block_on(super::cli::judge(&disk.updates, &disk.target));
                match (judged.version, judged.refusal) {
                    (None, None) => Armed::Nothing,
                    (Some(version), None) => Armed::Fits { version, applies: applies(inner.auto, disk.unit).into() },
                    (version, Some(why)) => Armed::Refused { version, why },
                }
            });
            Status(inner).write(|facts| facts.armed = armed);
        }));
        let _ = mine.await;
        *reading = None;
    }

    /// One write. A reader is woken only when the state told changed; the age of the last check is not a change.
    fn write(&self, change: impl FnOnce(&mut Facts)) {
        self.0.facts.send_if_modified(|facts| {
            let before = told(&facts.chase, &facts.armed, None);
            change(facts);
            told(&facts.chase, &facts.armed, None) != before
        });
    }
}

#[cfg(test)]
impl Status {
    /// For a test: each state a reader woken at a change is told, in order.
    pub(crate) fn told_at_each_change(&self) -> tokio::sync::mpsc::UnboundedReceiver<UpdateState> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (status, mut facts) = (self.clone(), self.0.facts.subscribe());
        tokio::spawn(async move {
            while facts.changed().await.is_ok() && tx.send(status.state()).is_ok() {}
        });
        rx
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
