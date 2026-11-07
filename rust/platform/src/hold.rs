//! The product half of the hold tool: a marked line a test may hold a thread at until it releases it (`test_hold` is the
//! test half). Both profiles expand a point to the same code, `if ON { arrive(..) }`, with `ON` the build's
//! `cfg!(debug_assertions)`, and `arrive` and `arrive_async` have one signature each with a body only under debug
//! assertions, so a release caller type-checks against the same signatures a debug caller does. `ON` is false in a
//! release build, so no key is evaluated, and the optimizer drops the call and the name (`strings` on a release build
//! shows none). Every test build is the dev profile and every shipped build is `--release`, so tests always have the
//! points and no installed binary has one. A point goes on a decision path (once per op, per attempt, per tick), never
//! in a per-byte or per-frame loop, and changes nothing when unarmed.
//!
//! A point's name is a string literal of `a-z 0-9 . _ -`, `<subsystem>.<place>`, checked at compile time:
//!
//! ```
//! sot_platform::hold_point!("doc.name.fine");
//! ```
//!
//! ```compile_fail,E0080
//! sot_platform::hold_point!("Doc Name Not Fine");
//! ```
//!
//! A key is anything `Display`, whatever its shape; this compiles and runs unarmed. (rustdoc's debug assertions are the doc
//! test crate's, while the library is built as the profile says, so `cargo test --release --doc` runs the release
//! expansion and bodies; no gate runs it, and the gate's `cargo check --release -p sot-chat` covers only what sot-chat calls.)
//!
//! ```
//! # use std::path::Path;
//! # fn maybe() -> Result<String, std::io::Error> { Ok("k".into()) }
//! # async fn key() -> String { "k".into() }
//! # async fn later() { sot_platform::hold_point_async!("doc.key.async", key().await); }
//! fn run(dir: &Path, only: u64) -> Result<(), std::io::Error> {
//!     sot_platform::hold_point!("doc.key.display", dir.display());
//!     sot_platform::hold_point!("doc.key.call", dir.to_string_lossy().len());
//!     sot_platform::hold_point!("doc.key.format", format!("k-{}", 7));
//!     sot_platform::hold_point!("doc.key.var", only);
//!     sot_platform::hold_point!("doc.key.try", maybe()?);
//!     Ok(())
//! }
//! run(Path::new("/x"), 3).unwrap();
//! # let _ = later();
//! ```

/// Whether this build has points.
#[doc(hidden)]
pub const ON: bool = cfg!(debug_assertions);

/// Marks a line in sync code (a thread, `spawn_blocking`); an optional second argument is a key (anything `Display`) a
/// test's arm may match, formatted only when an arm asks for a key. Held only while a test has armed the point.
#[macro_export]
macro_rules! hold_point {
    ($name:literal $(, $key:expr)? $(,)?) => {{
        const _: () = $crate::hold::check_name($name);
        if $crate::hold::ON {
            $crate::hold::arrive($name, $crate::__hold_key!($($key)?));
        }
    }};
}

/// Marks a line in async code: the arrival waits on `spawn_blocking`, never on a runtime thread.
#[macro_export]
macro_rules! hold_point_async {
    ($name:literal $(, $key:expr)? $(,)?) => {{
        const _: () = $crate::hold::check_name($name);
        if $crate::hold::ON {
            // Its own statement: the key's borrow must not live across the await.
            let arrival = $crate::hold::arrive_async($name, $crate::__hold_key!($($key)?));
            arrival.await;
        }
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __hold_key {
    () => { ::core::option::Option::<&dyn ::core::fmt::Display>::None };
    ($key:expr) => { ::core::option::Option::Some(&$key as &dyn ::core::fmt::Display) };
}

#[doc(hidden)]
pub const fn check_name(name: &str) {
    let bytes = name.as_bytes();
    assert!(!bytes.is_empty(), "a hold point's name is empty");
    let mut at = 0;
    while at < bytes.len() {
        assert!(
            matches!(bytes[at], b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'),
            "a hold point's name holds a byte outside a-z 0-9 . _ -"
        );
        at += 1;
    }
}

/// The call a point makes: one signature in both profiles, a body only with debug assertions.
#[doc(hidden)]
pub fn arrive(point: &'static str, key: Option<&dyn std::fmt::Display>) {
    #[cfg(debug_assertions)]
    imp::arrive(point, key);
    #[cfg(not(debug_assertions))]
    let _ = (point, key);
}

/// The async call a point makes: one signature in both profiles, a body only with debug assertions.
#[doc(hidden)]
pub fn arrive_async(point: &'static str, key: Option<&dyn std::fmt::Display>) -> impl std::future::Future<Output = ()> + Send + 'static {
    #[cfg(debug_assertions)]
    {
        imp::arrive_async(point, key)
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (point, key);
        std::future::ready(())
    }
}

#[cfg(all(debug_assertions, feature = "test-support"))]
pub use imp::{GIVE_UP, WAIT};
#[cfg(all(debug_assertions, feature = "test-support"))]
pub(crate) use imp::{arrivals, install, release, uninstall, wait_arrival};
#[cfg(all(debug_assertions, test))]
pub(crate) use imp::waiting_arms;

/// Nothing on the product side panics or refuses: whatever an arrival meets (a hold on a thread the test needs, a
/// release that never comes) is recorded on its ticket, and the arm fails the test by name at its next call or drop.
#[cfg(debug_assertions)]
#[cfg_attr(not(feature = "test-support"), allow(dead_code, reason = "the arm's callers are test_hold's, under feature test-support"))]
mod imp {
    use std::fmt::Display;
    use std::future::Future;
    use std::sync::{Arc, Condvar, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    /// The longest any test-side wait takes (`reached`, a second arm of a point).
    pub const WAIT: Duration = Duration::from_secs(30);
    /// The longest a held arrival waits; then it goes on and its arm fails the test by name.
    pub const GIVE_UP: Duration = Duration::from_secs(60);

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Ticket {
        Held,
        Released,
        Passed,
        GaveUp,
        /// Its waiter was lost before the wait ran or finished (a cancelled blocking task, a dropped runtime).
        Lost,
    }

    struct Slot {
        point: &'static str,
        arm: u64,
        key: Option<String>,
        hold: usize,
        give_up: Duration,
        tickets: Vec<Ticket>,
        /// Wakes this arm's waiters only, so no test's notify wakes another test's wait.
        changed: Arc<Condvar>,
    }

    struct State {
        slots: Vec<Slot>,
        next_arm: u64,
        /// Installs waiting for another arm of their point to end.
        waiting: usize,
    }

    static STATE: Mutex<State> = Mutex::new(State { slots: Vec::new(), next_arm: 0, waiting: 0 });
    static ENDED: Condvar = Condvar::new();

    /// A panic in a test must not poison the table for the tests beside it.
    fn lock() -> MutexGuard<'static, State> {
        STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn sleep_on<'a>(state: MutexGuard<'a, State>, on: &Condvar, left: Duration) -> MutexGuard<'a, State> {
        on.wait_timeout(state, left).unwrap_or_else(|poisoned| poisoned.into_inner()).0
    }

    /// A ticket that waits at its line.
    struct Held {
        arm: u64,
        ticket: usize,
        give_up: Duration,
        changed: Arc<Condvar>,
    }

    impl Held {
        /// Waits while the ticket is held, its arm lives and the give-up bound has not passed.
        fn wait(self) {
            let deadline = Instant::now() + self.give_up;
            let mut state = lock();
            loop {
                let Some(slot) = state.slots.iter_mut().find(|slot| slot.arm == self.arm) else { return };
                if slot.tickets[self.ticket] != Ticket::Held {
                    return;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    slot.tickets[self.ticket] = Ticket::GaveUp;
                    self.changed.notify_all();
                    return;
                }
                state = sleep_on(state, &self.changed, left);
            }
        }
    }

    impl Drop for Held {
        /// A wait that never ran (its blocking task cancelled with its runtime) leaves its ticket lost, to fail the arm.
        fn drop(&mut self) {
            let mut state = lock();
            if let Some(slot) = state.slots.iter_mut().find(|slot| slot.arm == self.arm) {
                if slot.tickets[self.ticket] == Ticket::Held {
                    slot.tickets[self.ticket] = Ticket::Lost;
                    self.changed.notify_all();
                }
            }
        }
    }

    /// Takes the next ticket at `point` if an arm matches the key; `Some` only when the ticket is held.
    fn claim(point: &'static str, key: Option<&dyn Display>) -> Option<Held> {
        let mut state = lock();
        let slot = state.slots.iter_mut().find(|slot| slot.point == point)?;
        if let Some(wanted) = &slot.key {
            if key.map(|key| key.to_string()).as_ref() != Some(wanted) {
                return None;
            }
        }
        let ticket = slot.tickets.len();
        let held = ticket < slot.hold;
        slot.tickets.push(if held { Ticket::Held } else { Ticket::Passed });
        slot.changed.notify_all();
        held.then(|| Held { arm: slot.arm, ticket, give_up: slot.give_up, changed: slot.changed.clone() })
    }

    pub fn arrive(point: &'static str, key: Option<&dyn Display>) {
        if let Some(held) = claim(point, key) {
            held.wait();
        }
    }

    /// The claim is made at the call, so the future holds no borrow of the key. A runtime dropped under the wait cancels
    /// its blocking task unrun; the `Drop` of `Held` then marks the ticket lost, so nothing goes on holding it.
    pub fn arrive_async(point: &'static str, key: Option<&dyn Display>) -> impl Future<Output = ()> + Send + 'static {
        let held = claim(point, key);
        async move {
            if let Some(held) = held {
                // Polled outside a tokio runtime there is no pool to wait on: wait as a sync point does.
                if tokio::runtime::Handle::try_current().is_ok() {
                    tokio::task::spawn_blocking(move || held.wait()).await.ok();
                } else {
                    held.wait();
                }
            }
        }
    }

    /// Installs an arm of `point`, waiting up to `WAIT` for another arm of it to end; returns the arm's id.
    pub(crate) fn install(point: &'static str, key: Option<String>, hold: usize, give_up: Duration) -> u64 {
        super::check_name(point);
        let deadline = Instant::now() + WAIT;
        let mut state = lock();
        state.waiting += 1;
        while state.slots.iter().any(|slot| slot.point == point) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                state.waiting -= 1;
                drop(state);
                panic!("hold point {point}: another arm of it did not end within {WAIT:?}");
            }
            state = sleep_on(state, &ENDED, left);
        }
        state.waiting -= 1;
        let arm = state.next_arm;
        state.next_arm += 1;
        let changed = Arc::new(Condvar::new());
        state.slots.push(Slot { point, arm, key, hold, give_up, tickets: Vec::new(), changed });
        arm
    }

    #[cfg(test)]
    pub(crate) fn waiting_arms() -> usize {
        lock().waiting
    }

    /// The arm's slot: an `Arm` borrows its id, so the slot lives as long as any caller.
    fn slot_of(state: &mut State, arm: u64) -> &mut Slot {
        state.slots.iter_mut().find(|slot| slot.arm == arm).expect("an arm's slot lives as long as the arm")
    }

    /// The failure a give-up or a lost waiter leaves on its arm, naming the point, the ticket and the usual cause.
    fn gave_up(slot: &Slot) -> Option<String> {
        let failed = slot.tickets.iter().position(|ticket| matches!(ticket, Ticket::GaveUp | Ticket::Lost))?;
        Some(if slot.tickets[failed] == Ticket::Lost {
            format!(
                "hold point {}: arrival {failed}'s waiter was lost before it finished (a cancelled blocking task or a dropped \
                 runtime), so the hold ended early",
                slot.point
            )
        } else {
            format!(
                "hold point {}: arrival {failed} was held past its {:?} bound and went on (if a sync point sits on a current-thread \
                 runtime's own thread, it stops the test that must release it: use hold_point_async!)",
                slot.point, slot.give_up
            )
        })
    }

    /// Ends an arm: every held arrival goes on and later arrivals pass. Returns its give-up failure, if any.
    pub(crate) fn uninstall(arm: u64) -> Option<String> {
        let mut state = lock();
        let at = state.slots.iter().position(|slot| slot.arm == arm)?;
        let slot = state.slots.remove(at);
        slot.changed.notify_all();
        ENDED.notify_all();
        gave_up(&slot)
    }

    pub(crate) fn arrivals(arm: u64) -> Result<usize, String> {
        let mut state = lock();
        let slot = slot_of(&mut state, arm);
        gave_up(slot).map_or(Ok(slot.tickets.len()), Err)
    }

    /// Waits until arrival `ticket` of the arm exists; the error names the point, the ticket and what was seen.
    pub(crate) fn wait_arrival(arm: u64, ticket: usize, bound: Duration) -> Result<(), String> {
        let deadline = Instant::now() + bound;
        let mut state = lock();
        loop {
            // A cancelled `reached_async` leaves this wait running after its arm is gone.
            let Some(slot) = state.slots.iter_mut().find(|slot| slot.arm == arm) else {
                return Err(format!("hold arm {arm} ended"));
            };
            if let Some(failure) = gave_up(slot) {
                return Err(failure);
            }
            if slot.tickets.len() > ticket {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "hold point {}: arrival {ticket} not reached within {bound:?}; {} arrivals seen (if this is a sync reached on a \
                     current-thread runtime's own thread, it can never see an arrival: use reached_async)",
                    slot.point,
                    slot.tickets.len()
                ));
            }
            let changed = slot.changed.clone();
            state = sleep_on(state, &changed, left);
        }
    }

    /// Lets held arrival `ticket` go on; refused if it has not arrived or is not held.
    pub(crate) fn release(arm: u64, ticket: usize) -> Result<(), String> {
        let mut state = lock();
        let slot = slot_of(&mut state, arm);
        if let Some(failure) = gave_up(slot) {
            return Err(failure);
        }
        match slot.tickets.get(ticket).copied() {
            None => Err(format!("hold point {}: release of arrival {ticket}, which has not arrived", slot.point)),
            Some(Ticket::Held) => {
                slot.tickets[ticket] = Ticket::Released;
                slot.changed.notify_all();
                Ok(())
            }
            Some(other) => Err(format!("hold point {}: release of arrival {ticket}, which is {other:?}, not held", slot.point)),
        }
    }
}
