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

/// Every change of the status is recorded with its value, in order, under the lock that made it: the record is never an
/// instruction to reread, and it never blocks the lock's holder, so an end that completes at once still leaves `ending`.
#[test]
fn every_change_is_recorded_in_order_with_its_value() {
    let shared = Shared::new();
    let (notes, record) = mpsc::channel();
    let initial = shared.set_announcer(notes);
    assert_eq!((initial.phase.as_str(), initial.run), ("starting", None));
    let (kill, _commands) = mpsc::channel();
    assert!(shared.run_begins("run-one".into(), kill, Arc::new(RunSignals::default())));
    let admission = shared.admit_end("run-one").expect("the end is admitted");
    shared.run_ended();
    assert_eq!(shared.wait_end(admission).outcome, EndRunOutcome::Ended);
    shared.admit_reset().expect("a resting supervisor admits a reset");
    shared.reset_done();
    shared.terminal("test".into());
    shared.close_announcer();
    let told: Vec<_> = record.try_iter().map(|s| (s.phase, s.run)).collect();
    let phases: Vec<_> = told.iter().map(|(phase, _)| phase.as_str()).collect();
    assert_eq!(phases, ["starting", "ending", "ended_no_respawn", "starting", "terminal"], "{told:?}");
    assert_eq!(told[0].1.as_deref(), Some("run-one"), "the first change names the run it began");
    assert!(told.iter().all(|(_, run)| run.as_deref() == Some("run-one")), "{told:?}");
}

/// Polling a status that did not change records nothing.
#[test]
fn polling_an_unchanged_status_records_nothing() {
    let shared = Shared::new();
    let (notes, record) = mpsc::channel();
    shared.set_announcer(notes);
    crate::row::Management::poll(&shared);
    assert!(record.try_recv().is_err(), "nothing changed, nothing recorded");
}

/// Typed input changes the status at most once a pace, to the wall clock's second when it was taken, and each change is
/// sent down the record once: a held key, a paste or a flood inside a pace costs one report and nothing per input. The
/// pace is counted on the monotonic clock, so two reports are never less than a pace apart, the first input at the pace
/// is recorded, and a wall clock that steps changes the second recorded and never the pace: a second recorded while the
/// clock was ahead is replaced at the next input a pace later. The time stays with the supervisor across runs, and every
/// later report carries it.
#[test]
fn typed_input_changes_the_status_at_most_once_a_pace() {
    let shared = Shared::new();
    let (notes, record) = mpsc::channel();
    assert_eq!(shared.set_announcer(notes).input_at, None, "nothing typed yet");
    let start = Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let pace = u64::try_from(INPUT_PACE.as_millis()).unwrap();
    let t = 1_767_225_600;
    let told = || record.try_iter().map(|s| s.input_at).collect::<Vec<_>>();
    // Ten thousand inputs inside one pace, across ten of the wall clock's seconds.
    for i in 0..10_000u64 {
        let millis = i * (pace - 1) / 9_999;
        shared.typed_at(at(millis), t + millis / 1000);
    }
    assert_eq!(told(), [Some(t)], "one change for the whole pace, at its first second");
    assert_eq!(crate::row::Management::status(&shared).input_at, Some(t));

    // A pace is ten whole seconds, not ten of the wall clock's seconds: 9.05 s after the last report is too soon.
    shared.typed_at(at(9_050), t + 10);
    assert_eq!(told(), [], "a report less than a pace after the one before");
    shared.typed_at(at(pace), t + 10);
    shared.typed_at(at(pace + 3_000), t + 13);
    assert_eq!(told(), [Some(t + 10)], "the first input at the pace, and no other");

    // The wall clock is three hours ahead for one input. The next input a pace later, the clock right again, replaces
    // that second, and typing counts throughout.
    shared.typed_at(at(2 * pace), t + 3 * 3600);
    shared.typed_at(at(2 * pace + 5_000), t + 25);
    shared.typed_at(at(3 * pace), t + 30);
    assert_eq!(told(), [Some(t + 3 * 3600), Some(t + 30)], "a clock that was ahead for one input");

    let (kill, _commands) = mpsc::channel();
    assert!(shared.run_begins("run-one".into(), kill, Arc::new(RunSignals::default())));
    let told: Vec<_> = record.try_iter().map(|s| (s.run, s.input_at)).collect();
    assert_eq!(told, [(Some("run-one".to_string()), Some(t + 30))], "a new run's report carries the time");
}
