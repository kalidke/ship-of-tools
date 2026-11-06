use super::*;
use crate::lifecycle::shutdown::terminal_retiring;
use sot_containment::Signal;

/// Far past any bound or runner slowness: a retire that waits for a stalled participant takes this long.
const STALL: Duration = Duration::from_secs(30);
/// Slack over a bound for a slow runner, well under [`STALL`].
const SLACK: Duration = Duration::from_secs(5);

type Events = Arc<Mutex<Vec<String>>>;

fn note(events: &Events, what: String) {
    events.lock().unwrap().push(what);
}

/// A fake participant that records each step; a `stall` makes its wait outlast any bound.
struct Fake {
    name: &'static str,
    events: Events,
    stall: bool,
}

impl Participant for Fake {
    fn name(&self) -> &'static str {
        self.name
    }
    fn close_admission(&self) {
        note(&self.events, format!("{}:closed", self.name));
    }
    fn request_stop(&self) {
        note(&self.events, format!("{}:stop", self.name));
    }
    fn wait_retired(&self, within: Duration) -> bool {
        if self.stall {
            std::thread::sleep(within + STALL);
            return false;
        }
        note(&self.events, format!("{}:done", self.name));
        true
    }
}

fn fake(name: &'static str, events: &Events, stall: bool) -> Arc<Fake> {
    Arc::new(Fake { name, events: events.clone(), stall })
}

/// Through the controlled end's own terminal: every admission closes before any stop is asked, every stop before the
/// completions, and the terminal comes after all of them with the code it was given.
#[test]
fn participants_close_stop_complete_and_then_the_terminal_runs() {
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let registry = Registry::new();
    let events: Events = Arc::default();
    assert!(registry.register(fake("a", &events, false)));
    assert!(registry.register(fake("b", &events, false)));
    let ev = events.clone();
    let code = terminal_retiring(&registry, signal, 0, |_| {}, move |code| {
        note(&ev, "terminal".into());
        code
    });
    assert_eq!(code, 0);
    assert_eq!(
        *events.lock().unwrap(),
        ["a:closed", "b:closed", "a:stop", "b:stop", "a:done", "b:done", "terminal"]
    );
    assert!(signal.is_fired());
    assert!(!registry.register(fake("late", &events, false)), "a registrant after retirement began must be refused");
}

/// A participant that never finishes is named incomplete within the bound, and the terminal still runs with its code.
#[test]
fn a_stalled_participant_is_reported_incomplete_within_the_bound_and_the_exit_stands() {
    let events: Events = Arc::default();
    let registry = Registry::new();
    registry.register(fake("ok", &events, false));
    registry.register(fake("stuck", &events, true));
    let began = std::time::Instant::now();
    let report = retire(&registry, Duration::from_millis(400));
    assert_eq!(report.incomplete, ["stuck"]);
    assert!(began.elapsed() < Duration::from_millis(400) + SLACK, "retirement took {:?}", began.elapsed());

    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let registry = Registry::new();
    registry.register(fake("stuck", &events, true));
    let began = std::time::Instant::now();
    let code = terminal_retiring(&registry, signal, 75, |_| {}, |code| code);
    assert_eq!(code, 75);
    assert!(
        began.elapsed() < crate::lifecycle::shutdown::FIRE_WAIT + crate::lifecycle::shutdown::FOLDERS_WAIT + SLACK,
        "the terminal took {:?}",
        began.elapsed()
    );
    assert!(signal.is_fired());
}

/// With nothing registered the report is empty and no thread is needed.
#[test]
fn no_participant_means_nothing_to_retire() {
    assert_eq!(retire(&Registry::new(), Duration::from_secs(1)), Retirement::default());
}
