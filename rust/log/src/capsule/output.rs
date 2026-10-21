//! Commit pacing and the bounded output budget between the reader thread and the writer loop.
use super::*;

const GROUP_COMMIT_WINDOW: Duration = Duration::from_millis(50);
pub(super) const GROUP_COMMIT_BYTES: usize = 256 * 1024;
/// The producer quiet this long with output pending means commit now.
const OUTPUT_IDLE: Duration = Duration::from_millis(2);
/// No idle commit sooner than this after the last fsync `flush_output` made (at most 100 idle commits/s).
const MIN_COMMIT_GAP: Duration = Duration::from_millis(10);

/// Whether the main loop commits pending output now: always at the group
/// window, and earlier when the producer has gone idle with output pending
/// and the last fsync is at least [`MIN_COMMIT_GAP`] old.
pub(super) fn should_flush_output(
    since_commit: Duration,
    pending_bytes: usize,
    since_output: Duration,
    since_fsync: Duration,
) -> bool {
    since_commit >= GROUP_COMMIT_WINDOW
        || (pending_bytes > 0 && since_output >= OUTPUT_IDLE && since_fsync >= MIN_COMMIT_GAP)
}

/// How long the main loop may wait before the earliest commit `should_flush_output` allows.
pub(super) fn output_wait(pending_bytes: usize, since_commit: Duration, since_output: Duration, since_fsync: Duration) -> Duration {
    if pending_bytes == 0 {
        return GROUP_COMMIT_WINDOW;
    }
    GROUP_COMMIT_WINDOW
        .saturating_sub(since_commit)
        .min(OUTPUT_IDLE.saturating_sub(since_output).max(MIN_COMMIT_GAP.saturating_sub(since_fsync)))
}

/// The output channel's byte budget (ADR 0041 budget table: "producer
/// channel 8 MiB bounded"). Bounds OUTSTANDING bytes only — chunks the
/// reader thread has reserved (in `READ_CHUNK`-sized units, before it ever
/// calls `read()`) but the writer loop has not yet committed — never a
/// second buffer of its own; see [`OutputBudget`].
const OUTPUT_QUEUE_BUDGET_BYTES: u64 = 8 * 1024 * 1024;

struct BudgetState {
    outstanding: u64,
    closed: bool,
    /// Test-only waiter-entry witness: incremented under the mutex
    /// immediately before `Condvar::wait`, decremented on wake. Lets a
    /// unit test PROVE a reserve entered the wait before releasing or
    /// cancelling — without it, a test's release can win the race to the
    /// first bound check and pass without ever exercising the wake path,
    /// so a missing `notify_all` could escape.
    #[cfg(test)]
    waiters: u32,
}

/// The bounded output budget (ADR 0041: "producer channel 8 MiB bounded —
/// when full the writer loop stops POLLING output... control/liveness
/// always serviced"). Implemented as a shared OUTSTANDING-byte counter
/// rather than a second queue structure: `ReaderEvent::Output` chunks still
/// ride the same channel the reader thread always used, but that thread
/// blocks (via this condvar) BEFORE its next `read()` once the budget is
/// exhausted — so `Command`, read by the writer loop from a completely
/// separate channel, is never gated by it at all.
pub(super) struct OutputBudget {
    state: Mutex<BudgetState>,
    space_available: Condvar,
}

impl OutputBudget {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(BudgetState {
                outstanding: 0,
                closed: false,
                #[cfg(test)]
                waiters: 0,
            }),
            space_available: Condvar::new(),
        }
    }

    /// Reader-thread-only: reserves `n` bytes BEFORE the read that will
    /// produce them, blocking while `outstanding + n` would exceed the
    /// budget — never after the read, and never against `outstanding`
    /// alone. Returns `false` if the budget was (or became, while
    /// waiting) cancelled — the reader must stop immediately, WITHOUT
    /// reserving, and never call `read()` again.
    pub(super) fn reserve(&self, n: u64) -> bool {
        let mut g = self.state.lock().unwrap();
        loop {
            if g.closed {
                return false;
            }
            if g.outstanding + n <= OUTPUT_QUEUE_BUDGET_BYTES {
                g.outstanding += n;
                return true;
            }
            #[cfg(test)]
            {
                g.waiters += 1;
            }
            g = self.space_available.wait(g).unwrap();
            #[cfg(test)]
            {
                g.waiters -= 1;
            }
        }
    }

    /// Writer-loop-only: releases exactly `n` bytes once they have been
    /// accounted for (a frame appended, or reservation given back unused).
    /// Checked, not saturating: a mismatch here is this
    /// module's own bookkeeping bug and must panic loudly, never silently
    /// absorb the discrepancy.
    pub(super) fn release(&self, n: u64) {
        let mut g = self.state.lock().unwrap();
        g.outstanding =
            g.outstanding.checked_sub(n).expect("OutputBudget::release: released more than was reserved");
        self.space_available.notify_all();
    }

    /// Cancels the budget — every future and currently-blocked `reserve`
    /// call returns `false` immediately. Called from `BudgetCancelGuard`'s
    /// `Drop` on every exit from `run` (a reader blocked in
    /// `reserve` must never be able to outlive the writer loop that is the
    /// only thing that would otherwise ever release it).
    fn cancel(&self) {
        let mut g = self.state.lock().unwrap();
        g.closed = true;
        self.space_available.notify_all();
    }
}

/// RAII: cancels the shared output budget on ANY exit from `run` — a
/// normal return, an early `?`, or a panic unwind. See `OutputBudget::cancel`.
pub(super) struct BudgetCancelGuard(pub(super) Arc<OutputBudget>);
impl Drop for BudgetCancelGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}


/// `OutputBudget`'s blocking is proven HERE, deterministically, because it
/// cannot be proven end-to-end: whether the e2e flood ever fills the budget
/// depends on conhost's burst pacing on the host machine, which no test
/// controls — a runner-image change turned exactly that e2e assertion red
/// on unchanged code. The flood test keeps the properties that are always
/// true (no deadlock, verify-green, bookkeeping live); the bound itself is
/// a plain condvar protocol, provable right at the primitive.
// These unit tests open a real store, so they run wherever the store has a
// real rename arm. That was Linux and Windows only until M1 gave `host`
// its `renamex_np` arm; macOS is now a third, and the gate says so. Every
// OTHER Unix still hits `host`'s fail-closed arm, which is why this is
// three named targets and not bare `any(unix, windows)`. Nothing in this
// module reads `/proc`, opens a pidfd or expects PDEATHSIG — it is a
// condvar protocol and a run-end marker — so no test inside needed a
// narrower gate of its own before this one widened.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[path = "output_tests.rs"]
mod tests;
