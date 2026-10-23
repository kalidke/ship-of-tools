//! `Shared`'s own unit tests.
use super::*;

/// A supervisor that goes terminal with an agent running ends that agent: the run's channel would otherwise just be
/// dropped (a disconnect is not a kill), leaving a healthy agent under a supervisor that can no longer serve its row.
#[test]
fn a_terminal_supervisor_ends_its_running_agent() {
    let shared = Shared::new();
    let (kill, commands) = mpsc::channel();
    assert!(shared.run_begins("run-one".into(), kill, Arc::new(RunSignals::default())));
    shared.terminal("the supervisor lane failed".into());
    assert!(
        matches!(commands.try_recv(), Ok(Command::Stop) | Ok(Command::Kill)),
        "the run was told to end before it was dropped"
    );
}

/// A listener that cannot be bound again while the supervisor rests wakes the main thread: nothing else can ever stop
/// or reset it, and it holds its fence.
#[test]
fn a_lost_row_listener_wakes_a_resting_supervisor() {
    let shared = Arc::new(Shared::new());
    shared.run_ended();
    let (done, woke) = mpsc::channel();
    let waiter = Arc::clone(&shared);
    std::thread::spawn(move || {
        let _ = waiter.wait_after_run();
        let _ = done.send(());
    });
    std::thread::sleep(Duration::from_millis(300));
    shared.row_lost();
    assert!(woke.recv_timeout(Duration::from_secs(2)).is_ok(), "a resting supervisor kept waiting after its listener was lost");
}

/// No agent starts once the listener is lost: a run admitted between the lost check and its admission would be
/// unreachable.
#[test]
fn a_run_is_not_admitted_once_the_row_listener_is_lost() {
    let shared = Shared::new();
    shared.row_lost();
    let (kill, _commands) = mpsc::channel();
    assert!(!shared.run_begins("run-one".into(), kill, Arc::new(RunSignals::default())));
}

/// An admitted end's answer belongs to its own run: after run A ended, a reset and run B's admission leave A's answer
/// for A's waiter, however late that waiter reads it.
#[test]
fn a_completion_answer_belongs_to_the_run_it_was_admitted_for() {
    let shared = Arc::new(Shared::new());
    let begin = |id: &str| {
        let (kill, commands) = mpsc::channel();
        assert!(shared.run_begins(id.into(), kill, Arc::new(RunSignals::default())));
        commands
    };
    let _a = begin("run-a");
    let first = shared.admit_end("run-a").expect("run A's end is admitted");
    shared.run_ended();
    // The reset and the next run come before A's waiter has read its answer.
    let resetter = Arc::clone(&shared);
    let reset = std::thread::spawn(move || resetter.reset());
    let After::Reset = shared.wait_after_run() else { panic!("a reset was admitted") };
    shared.reset_done();
    assert_eq!(reset.join().unwrap().outcome, ResetOutcome::Reset);
    let _b = begin("run-b");
    let second = shared.admit_end("run-b").expect("run B's end is admitted");
    assert_eq!(shared.wait_end(first).outcome, EndRunOutcome::Ended, "run A's waiter got its own answer");
    shared.run_ended();
    assert_eq!(shared.wait_end(second).outcome, EndRunOutcome::Ended);
}

/// The daemon's cell orders a supervisor's runs by their ids: one supervisor mints each run's id with `now_v7`, so every
/// id is strictly greater than the last, whatever the clock's resolution.
#[test]
fn runs_minted_by_one_supervisor_order_by_time() {
    let mut last = uuid::Uuid::now_v7();
    for _ in 0..10_000 {
        let next = uuid::Uuid::now_v7();
        assert!(next > last, "{next} does not order after {last}");
        last = next;
    }
}

/// An end is admitted only while the run is open: `close_run` closes admission and reports, under the same lock, whether
/// one was admitted, so every end answered `ended` is one the caller recorded.
#[test]
fn an_end_admitted_after_the_run_is_closed_is_refused_and_one_before_is_reported() {
    let shared = Shared::new();
    let (kill, _commands) = mpsc::channel();
    assert!(shared.run_begins("run-a".into(), kill, Arc::new(RunSignals::default())));
    shared.admit_end("run-a").expect("an end before the close is admitted");
    assert!(shared.close_run(), "the admitted end is reported");

    let other = Shared::new();
    let (kill, _commands) = mpsc::channel();
    assert!(other.run_begins("run-b".into(), kill, Arc::new(RunSignals::default())));
    assert!(!other.close_run(), "no end was admitted");
    let answer = other.admit_end("run-b").expect_err("an end after the close is refused");
    assert_eq!(answer.outcome, EndRunOutcome::Failed);
}

/// A terminal supervisor starts no run: its phase and its run stay as they were.
#[test]
fn a_terminal_supervisor_admits_no_run() {
    let shared = Shared::new();
    shared.terminal("test".into());
    let (kill, _commands) = mpsc::channel();
    assert!(!shared.run_begins("run-a".into(), kill, Arc::new(RunSignals::default())));
    assert!(shared.is_terminal());
}
