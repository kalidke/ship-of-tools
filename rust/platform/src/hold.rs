//! The product half of the hold tool: a marked line a test may hold a thread at until it releases it (`test_hold` is the
//! test half). A build without debug assertions has no point: the macros expand to `()`, so no call is made, no
//! argument is evaluated and no name is kept. Every test build is the dev profile and every shipped build is
//! `--release`, so tests always have the points and no installed binary has one. A point goes on a decision path (once
//! per op, per attempt, per tick), never in a per-byte or per-frame loop, and changes nothing when unarmed.
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

/// Marks a line in sync code (a thread, `spawn_blocking`); an optional second argument is a key (anything `Display`) a
/// test's arm may match, formatted only when an arm asks for a key. Held only while a test has armed the point.
#[macro_export]
macro_rules! hold_point {
    ($name:literal $(, $key:expr)? $(,)?) => {{
        #[cfg(debug_assertions)]
        {
            const _: () = $crate::hold::check_name($name);
            $crate::hold::arrive($name, $crate::__hold_key!($($key)?));
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = || ($( &$key, )?);
        }
    }};
}

/// Marks a line in async code: the arrival waits on `spawn_blocking`, never on a runtime thread.
#[macro_export]
macro_rules! hold_point_async {
    ($name:literal $(, $key:expr)? $(,)?) => {{
        #[cfg(debug_assertions)]
        {
            const _: () = $crate::hold::check_name($name);
            // Its own statement: the key's borrow must not live across the await.
            let arrival = $crate::hold::arrive_async($name, $crate::__hold_key!($($key)?));
            arrival.await;
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = || ($( &$key, )?);
        }
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __hold_key {
    () => { ::core::option::Option::<&dyn ::core::fmt::Display>::None };
    ($key:expr) => { ::core::option::Option::Some(&$key as &dyn ::core::fmt::Display) };
}

#[cfg(debug_assertions)]
pub use imp::{arrive, arrive_async, check_name};
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

    /// The claim is made at the call, so the future holds no borrow of the key; the wait is on `spawn_blocking`.
    pub fn arrive_async(point: &'static str, key: Option<&dyn Display>) -> impl Future<Output = ()> + Send + 'static {
        let held = claim(point, key);
        async move {
            if let Some(held) = held {
                tokio::task::spawn_blocking(move || held.wait()).await.ok();
            }
        }
    }

    /// Installs an arm of `point`, waiting up to `WAIT` for another arm of it to end; returns the arm's id.
    pub(crate) fn install(point: &'static str, key: Option<String>, hold: usize, give_up: Duration) -> u64 {
        check_name(point);
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

    /// The failure a give-up leaves on its arm, naming the point, the ticket and the usual cause.
    fn gave_up(slot: &Slot) -> Option<String> {
        let ticket = slot.tickets.iter().position(|ticket| *ticket == Ticket::GaveUp)?;
        Some(format!(
            "hold point {}: arrival {ticket} was held past its {:?} bound and went on (a sync point on a current-thread \
             runtime's own thread stops the test that must release it: use hold_point_async!)",
            slot.point, slot.give_up
        ))
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
            let slot = slot_of(&mut state, arm);
            if let Some(failure) = gave_up(slot) {
                return Err(failure);
            }
            if slot.tickets.len() > ticket {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "hold point {}: arrival {ticket} not reached within {bound:?}; {} arrivals seen (a sync reached on a \
                     current-thread runtime's own thread can never see an arrival: use reached_async)",
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
