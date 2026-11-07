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
//! ```compile_fail
//! sot_platform::hold_point!("Doc Name Not Fine");
//! ```

/// Marks a line in sync code (a thread, `spawn_blocking`); an optional second argument is a key (anything `Display`) a
/// test's arm may match. Held only while a test has armed the point.
#[macro_export]
macro_rules! hold_point {
    ($name:literal $(, $key:expr)? $(,)?) => {{
        #[cfg(debug_assertions)]
        {
            const _: () = $crate::hold::check_name($name);
            $crate::hold::arrive($name, $crate::__hold_key!($($key)?));
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
            $crate::hold::arrive_async($name, $crate::__hold_key!($($key)?)).await;
        }
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __hold_key {
    () => { ::core::option::Option::None };
    ($key:expr) => { ::core::option::Option::Some(::std::string::ToString::to_string(&$key)) };
}

#[cfg(debug_assertions)]
pub use imp::{arrive, arrive_async, check_name};
#[cfg(all(debug_assertions, feature = "test-support"))]
pub(crate) use imp::{arrivals, install, own_thread_of_current_thread_runtime, release, uninstall, wait_arrival};
#[cfg(all(debug_assertions, test))]
pub(crate) use imp::waiting_arms;
#[cfg(debug_assertions)]
pub use imp::{GIVE_UP, WAIT};

#[cfg(debug_assertions)]
#[cfg_attr(not(feature = "test-support"), allow(dead_code, reason = "the arm's callers are test_hold's, under feature test-support"))]
mod imp {
    use std::sync::{Condvar, Mutex, MutexGuard};
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
    }

    struct State {
        slots: Vec<Slot>,
        next_arm: u64,
        waiting: Vec<&'static str>,
    }

    static STATE: Mutex<State> = Mutex::new(State { slots: Vec::new(), next_arm: 0, waiting: Vec::new() });
    static CHANGED: Condvar = Condvar::new();

    /// A panic in a test must not poison the table for the tests beside it.
    fn lock() -> MutexGuard<'static, State> {
        STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn sleep_on<'a>(state: MutexGuard<'a, State>, left: Duration) -> MutexGuard<'a, State> {
        CHANGED.wait_timeout(state, left).unwrap_or_else(|poisoned| poisoned.into_inner()).0
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

    /// True on the thread that drives a current-thread runtime (a test body under `block_on`, a task): a hold there
    /// would stop the very test that must release it. A blocking-pool thread of that runtime is not, told by tokio's default thread name (it has no other mark).
    pub(crate) fn own_thread_of_current_thread_runtime() -> bool {
        use tokio::runtime::{Handle, RuntimeFlavor};
        Handle::try_current().is_ok_and(|handle| handle.runtime_flavor() == RuntimeFlavor::CurrentThread)
            && !matches!(std::thread::current().name(), Some("tokio-rt-worker" | "tokio-runtime-worker"))
    }

    /// Takes the next ticket at `point` if an arm matches the key; `Some` only when the ticket is held.
    fn claim(point: &'static str, key: &Option<String>) -> Option<(u64, usize, Duration)> {
        let mut state = lock();
        let slot = state.slots.iter_mut().find(|slot| slot.point == point)?;
        if slot.key.is_some() && slot.key != *key {
            return None;
        }
        let ticket = slot.tickets.len();
        let held = ticket < slot.hold;
        slot.tickets.push(if held { Ticket::Held } else { Ticket::Passed });
        let found = (slot.arm, ticket, slot.give_up);
        CHANGED.notify_all();
        held.then_some(found)
    }

    /// Waits while the ticket is held, its arm lives and the give-up bound has not passed.
    fn wait_held(arm: u64, ticket: usize, give_up: Duration) {
        let deadline = Instant::now() + give_up;
        let mut state = lock();
        loop {
            let Some(slot) = state.slots.iter_mut().find(|slot| slot.arm == arm) else { return };
            if slot.tickets[ticket] != Ticket::Held {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                slot.tickets[ticket] = Ticket::GaveUp;
                CHANGED.notify_all();
                return;
            }
            state = sleep_on(state, left);
        }
    }

    pub fn arrive(point: &'static str, key: Option<String>) {
        let Some((arm, ticket, give_up)) = claim(point, &key) else { return };
        if own_thread_of_current_thread_runtime() {
            release(arm, ticket).ok();
            panic!("hold point {point}: a sync point held on a current-thread runtime's own thread; use hold_point_async!");
        }
        wait_held(arm, ticket, give_up);
    }

    pub async fn arrive_async(point: &'static str, key: Option<String>) {
        let Some((arm, ticket, give_up)) = claim(point, &key) else { return };
        tokio::task::spawn_blocking(move || wait_held(arm, ticket, give_up)).await.ok();
    }

    /// Installs an arm of `point`, waiting up to `WAIT` for another arm of it to end; returns the arm's id.
    pub(crate) fn install(point: &'static str, key: Option<String>, hold: usize, give_up: Duration) -> u64 {
        check_name(point);
        let deadline = Instant::now() + WAIT;
        let mut state = lock();
        let mut queued = false;
        while state.slots.iter().any(|slot| slot.point == point) {
            if !queued {
                state.waiting.push(point);
                queued = true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                dequeue(&mut state, point);
                drop(state);
                panic!("hold point {point}: another arm of it did not end within {WAIT:?}");
            }
            state = sleep_on(state, left);
        }
        if queued {
            dequeue(&mut state, point);
        }
        let arm = state.next_arm;
        state.next_arm += 1;
        state.slots.push(Slot { point, arm, key, hold, give_up, tickets: Vec::new() });
        arm
    }

    fn dequeue(state: &mut State, point: &'static str) {
        if let Some(at) = state.waiting.iter().position(|waiting| *waiting == point) {
            state.waiting.remove(at);
        }
    }

    #[cfg(test)]
    pub(crate) fn waiting_arms(point: &'static str) -> usize {
        lock().waiting.iter().filter(|waiting| **waiting == point).count()
    }

    pub(crate) fn arrivals(arm: u64) -> usize {
        lock().slots.iter().find(|slot| slot.arm == arm).map_or(0, |slot| slot.tickets.len())
    }

    /// Ends an arm: every held arrival goes on and later arrivals pass. Returns the tickets that gave up.
    pub(crate) fn uninstall(arm: u64) -> Vec<usize> {
        let mut state = lock();
        let Some(at) = state.slots.iter().position(|slot| slot.arm == arm) else { return Vec::new() };
        let slot = state.slots.remove(at);
        CHANGED.notify_all();
        (0..slot.tickets.len()).filter(|ticket| slot.tickets[*ticket] == Ticket::GaveUp).collect()
    }

    fn gave_up_message(slot: &Slot, ticket: usize) -> String {
        format!("hold point {}: arrival {ticket} was held past its {:?} bound and went on", slot.point, slot.give_up)
    }

    /// Waits until arrival `ticket` of the arm exists; the error names the point, the ticket and what was seen.
    pub(crate) fn wait_arrival(arm: u64, ticket: usize, bound: Duration) -> Result<(), String> {
        let deadline = Instant::now() + bound;
        let mut state = lock();
        loop {
            let Some(slot) = state.slots.iter().find(|slot| slot.arm == arm) else {
                return Err(format!("hold arm {arm} is gone"));
            };
            if let Some(gave_up) = slot.tickets.iter().position(|t| *t == Ticket::GaveUp) {
                return Err(gave_up_message(slot, gave_up));
            }
            if slot.tickets.len() > ticket {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "hold point {}: arrival {ticket} not reached within {bound:?}; {} arrivals seen",
                    slot.point,
                    slot.tickets.len()
                ));
            }
            state = sleep_on(state, left);
        }
    }

    /// Lets held arrival `ticket` go on; refused if it has not arrived, is not held, or gave up.
    pub(crate) fn release(arm: u64, ticket: usize) -> Result<(), String> {
        let mut state = lock();
        let Some(slot) = state.slots.iter_mut().find(|slot| slot.arm == arm) else {
            return Err(format!("hold arm {arm} is gone"));
        };
        match slot.tickets.get(ticket).copied() {
            None => Err(format!("hold point {}: release of arrival {ticket}, which has not arrived", slot.point)),
            Some(Ticket::Held) => {
                slot.tickets[ticket] = Ticket::Released;
                CHANGED.notify_all();
                Ok(())
            }
            Some(Ticket::GaveUp) => Err(gave_up_message(slot, ticket)),
            Some(other) => Err(format!("hold point {}: release of arrival {ticket}, which is {other:?}, not held", slot.point)),
        }
    }
}
