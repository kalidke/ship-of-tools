//! The storage wait: a full state-root volume holds the authority; it never charges the crash counter or goes terminal.

use super::*;
use crate::host::storage_exhaustion;

/// A probe still running this long after it started ends the wait Terminal.
const PROBE_WATCHDOG: Duration = Duration::from_secs(60);

/// What a leg's death says about storage, from the exit status its owner read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LegDeath {
    /// The leg exited 71: its run failed with storage exhaustion.
    Storage,
    /// Any other status: ordinary crash accounting.
    Ordinary,
    /// No status (an adopted leg, or a status already taken): one durable probe decides.
    Unknown,
}

pub(super) fn leg_death(status: Option<ExitStatus>) -> LegDeath {
    match status {
        Some(ExitStatus::Code(c)) if c as i32 == crate::capsule::EXIT_LEG_STORAGE_FULL => {
            LegDeath::Storage
        }
        Some(_) => LegDeath::Ordinary,
        None => LegDeath::Unknown,
    }
}

/// What the authority does once storage is back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Resume {
    /// A leg died with storage exhaustion: spawn the next leg.
    Respawn,
    /// A leg died with no status: the first probe decides whether this was storage.
    Suspect,
    /// A recovery, end_run or reset worker failed with storage exhaustion: run startup recovery again.
    Recover { first_leg: bool },
}

struct InFlight {
    rx: mpsc::Receiver<std::io::Result<()>>,
    handle: JoinHandle<()>,
    started_at: Instant,
}

/// One storage wait. At most one probe worker runs at a time.
pub(super) struct Wait {
    resume: Resume,
    next_probe_at: Option<Instant>,
    probe_fn: fn(&Path) -> std::io::Result<()>,
    in_flight: Option<InFlight>,
    noted: bool,
}

impl Wait {
    pub(super) fn new(resume: Resume) -> Self {
        Self::with_probe(resume, probe)
    }

    fn with_probe(resume: Resume, probe_fn: fn(&Path) -> std::io::Result<()>) -> Self {
        Self {
            resume,
            next_probe_at: None,
            probe_fn,
            in_flight: None,
            noted: false,
        }
    }

    #[cfg(test)]
    pub(super) fn resume(&self) -> Resume {
        self.resume
    }

    /// The in-flight probe's thread, for a jump to Terminal from outside the wait.
    pub(super) fn take_worker_handle(&mut self) -> Option<JoinHandle<()>> {
        self.in_flight.take().map(|flight| flight.handle)
    }
}

pub(super) enum Outcome {
    Waiting(Wait),
    Resume(Resume),
    /// A suspected leg death whose probe passed: ordinary accounting.
    NotStorage,
    Terminal(String),
}

/// Seconds between probes: 1, 2, 4, 8, 16 for steps 0 to 4, then 30.
pub(super) fn delay(step: u32) -> Duration {
    Duration::from_secs(match step {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        _ => 30,
    })
}

/// Today's crash accounting for a leg that ended with no storage cause: an
/// unstable leg counts, a stable one zeroes the counter and the storage step.
pub(super) fn account(counter: &mut u32, step: &mut u32, unstable: bool) {
    if unstable {
        *counter += 1;
    } else {
        *counter = 0;
        *step = 0;
    }
}

/// One tick of the wait. A probe is a worker thread; its result is read on a
/// later tick. The first tick probes at once for `Suspect` and otherwise at
/// `now + delay(*step)`. A success is `NotStorage` for `Suspect` and
/// `Resume(resume)` otherwise, with the step kept; a recognized error moves
/// the step on and waits again (a `Suspect` is then a known storage death);
/// anything else ends the wait Terminal.
pub(super) fn advance(mut wait: Wait, step: &mut u32, state_dir: &Path, now: Instant) -> Outcome {
    if let Some(flight) = wait.in_flight.take() {
        match flight.rx.try_recv() {
            Ok(result) => {
                join_and_warn(flight.handle, "storage probe");
                return match result {
                    Ok(()) if wait.resume == Resume::Suspect => Outcome::NotStorage,
                    Ok(()) => Outcome::Resume(wait.resume),
                    Err(e) => probe_failed(wait, step, e, now),
                };
            }
            Err(mpsc::TryRecvError::Empty) => {
                if watchdog_expired(flight.started_at, PROBE_WATCHDOG, now) {
                    abandon_worker(flight.handle, "storage probe");
                    return Outcome::Terminal("the storage probe ran past its 60 s bound".into());
                }
                wait.in_flight = Some(flight);
                return Outcome::Waiting(wait);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                join_and_warn(flight.handle, "storage probe");
                return Outcome::Terminal(
                    "the storage probe thread ended without a result (possible panic)".into(),
                );
            }
        }
    }
    let due = match wait.next_probe_at {
        Some(at) => now >= at,
        None if wait.resume == Resume::Suspect => true,
        None => {
            wait.next_probe_at = Some(now + delay(*step));
            false
        }
    };
    if due {
        let (tx, rx) = mpsc::channel();
        let probe_fn = wait.probe_fn;
        let dir = state_dir.to_path_buf();
        let handle = std::thread::spawn(move || {
            let _ = tx.send(probe_fn(&dir));
        });
        wait.next_probe_at = None;
        wait.in_flight = Some(InFlight {
            rx,
            handle,
            started_at: now,
        });
    }
    Outcome::Waiting(wait)
}

fn probe_failed(mut wait: Wait, step: &mut u32, e: std::io::Error, now: Instant) -> Outcome {
    let error = crate::Error::Io(e);
    match storage_exhaustion(&error) {
        Some(code) => {
            if !wait.noted {
                wait.noted = true;
                note(format_args!("the state root's storage is exhausted (os error {code}); holding until a probe succeeds"));
            }
            *step += 1;
            if wait.resume == Resume::Suspect {
                wait.resume = Resume::Respawn;
            }
            wait.next_probe_at = Some(now + delay(*step));
            Outcome::Waiting(wait)
        }
        None => Outcome::Terminal(bounded_detail(format!(
            "the storage probe failed for another reason: {error}"
        ))),
    }
}

/// A real durable write to the state root: a new 4 KiB file written, synced
/// and removed, then the root synced. After the file exists, any error removes
/// it before the first error is returned; a name this did not create is never
/// removed.
pub(super) fn probe(state_dir: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(std::io::Error::from)?;
    let path = state_dir.join(format!(".storage-probe-{:016x}", u64::from_le_bytes(nonce)));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let written = file.write_all(&[0u8; 4096]).and_then(|()| file.sync_all());
    drop(file);
    let removed = std::fs::remove_file(&path);
    written?;
    removed?;
    crate::host::fsync_dir(state_dir).map_err(|e| match e {
        crate::Error::Io(e) => e,
        other => std::io::Error::other(other),
    })
}

/// Runs `f` until it stops failing with storage exhaustion, sleeping
/// `delay(step)` between tries (1, 2, 4 s and on); any other error returns at
/// once. For the steps before the lane binds, which have nothing to hold yet.
pub(super) fn retry_bootstrap<T>(
    what: &str,
    f: impl FnMut() -> crate::Result<T>,
) -> crate::Result<T> {
    retry_with(what, f, std::thread::sleep)
}

fn retry_with<T>(
    what: &str,
    mut f: impl FnMut() -> crate::Result<T>,
    mut sleep: impl FnMut(Duration),
) -> crate::Result<T> {
    let mut step = 0;
    loop {
        match f() {
            Err(e) => match storage_exhaustion(&e) {
                Some(code) => {
                    if step == 0 {
                        note(format_args!("{what}: the state root's storage is exhausted (os error {code}); retrying"));
                    }
                    sleep(delay(step));
                    step += 1;
                }
                None => return Err(e),
            },
            ok => return ok,
        }
    }
}

#[cfg(test)]
mod tests;
